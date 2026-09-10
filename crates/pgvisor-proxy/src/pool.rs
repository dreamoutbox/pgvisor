use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use pgvisor_core::protocol::message::{BackendMessage, InitialClientMessage, StartupMessage};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct FailoverConfig {
    pub failover_timeout: Duration,
    pub retry_interval: Duration,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            failover_timeout: Duration::from_secs(10),
            retry_interval: Duration::from_millis(100),
        }
    }
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("I/O error connecting to backend: {0}")]
    Io(#[from] std::io::Error),

    #[error("Pool exhausted or backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("Failover timeout exceeded waiting for healthy backend: {0}")]
    FailoverTimeout(String),
}

/// Node routing target for Postgres operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRole {
    Leader,
    Standby,
}

/// A live backend TCP connection managed by the pool.
pub struct PooledConnection {
    pub stream: TcpStream,
    pub role: BackendRole,
    pub addr: String,
    pub created_at: Instant,
}

impl std::fmt::Debug for PooledConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConnection")
            .field("role", &self.role)
            .field("addr", &self.addr)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl PooledConnection {
    /// Connects to a PostgreSQL instance and performs the wire protocol startup handshake.
    pub async fn connect(
        addr: &str,
        role: BackendRole,
        user: Option<&str>,
        database: Option<&str>,
    ) -> Result<Self, PoolError> {
        debug!(addr, ?role, "Opening new backend connection");
        let mut stream = TcpStream::connect(addr).await?;

        // 1. Send StartupMessage to backend
        let mut startup_buf = BytesMut::new();
        let mut params = HashMap::new();
        params.insert("user".to_string(), user.unwrap_or("postgres").to_string());
        params.insert(
            "database".to_string(),
            database.unwrap_or("postgres").to_string(),
        );
        params.insert("client_encoding".to_string(), "UTF8".to_string());

        let startup = StartupMessage {
            protocol_version: 196608,
            parameters: params,
        };
        InitialClientMessage::encode_startup(&startup, &mut startup_buf);
        stream.write_all(&startup_buf).await?;

        // 2. Consume backend authentication and status packets until ReadyForQuery ('Z')
        let mut read_buf = BytesMut::with_capacity(4096);
        loop {
            let n = stream.read_buf(&mut read_buf).await?;
            if n == 0 {
                return Err(PoolError::BackendUnavailable(format!(
                    "Backend at {} unexpectedly closed connection during startup handshake",
                    addr
                )));
            }

            while let Some(msg) = BackendMessage::decode(&mut read_buf)? {
                match msg {
                    BackendMessage::ReadyForQuery { .. } => {
                        debug!(
                            addr,
                            "Backend startup handshake completed, ready for query execution"
                        );
                        return Ok(Self {
                            stream,
                            role,
                            addr: addr.to_string(),
                            created_at: Instant::now(),
                        });
                    }
                    BackendMessage::ErrorResponse { message } => {
                        return Err(PoolError::BackendUnavailable(format!(
                            "Backend at {} rejected startup handshake: {}",
                            addr, message
                        )));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Thread-safe transaction-level connection pool.
#[derive(Clone)]
pub struct ConnectionPool {
    inner: Arc<Mutex<PoolInner>>,
    topology_notifier: Arc<watch::Sender<u64>>,
}

struct PoolInner {
    leader_addr: Option<String>,
    standby_addrs: Vec<String>,
    standby_rr_index: usize,
    idle_leaders: VecDeque<PooledConnection>,
    idle_standbys: HashMap<String, VecDeque<PooledConnection>>,
    max_idle_per_node: usize,
    version: u64,
}

impl ConnectionPool {
    pub fn new(max_idle_per_node: usize) -> Self {
        let (tx, _rx) = watch::channel(0);
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                leader_addr: None,
                standby_addrs: Vec::new(),
                standby_rr_index: 0,
                idle_leaders: VecDeque::new(),
                idle_standbys: HashMap::new(),
                max_idle_per_node,
                version: 0,
            })),
            topology_notifier: Arc::new(tx),
        }
    }

    /// Updates current cluster topology known from consensus.
    pub async fn update_topology(&self, leader: Option<String>, standbys: Vec<String>) {
        let mut inner = self.inner.lock().await;

        // If leader changed, drain stale leader connections immediately to prevent split-brain writes.
        if inner.leader_addr != leader {
            info!(old = ?inner.leader_addr, new = ?leader, "Leader topology changed, draining idle leader pool");
            inner.idle_leaders.clear();
            inner.leader_addr = leader;
        }

        // Retain only idle connections for standbys that remain in the updated topology
        inner
            .idle_standbys
            .retain(|addr, _| standbys.contains(addr));

        inner.standby_addrs = standbys;
        inner.standby_rr_index = 0;
        inner.version = inner.version.wrapping_add(1);
        let _ = self.topology_notifier.send(inner.version);
    }

    /// Flushes all idle connections to both leader and standbys (e.g. after cluster restore).
    pub async fn drain_all(&self) {
        let mut inner = self.inner.lock().await;
        inner.idle_leaders.clear();
        inner.idle_standbys.clear();
        inner.version = inner.version.wrapping_add(1);
        let _ = self.topology_notifier.send(inner.version);
    }

    /// Returns a receiver for topology updates.
    pub fn subscribe_topology(&self) -> watch::Receiver<u64> {
        self.topology_notifier.subscribe()
    }

    /// Acquires a connection for a specific role and user/database credentials.
    pub async fn acquire_for(
        &self,
        role: BackendRole,
        user: Option<&str>,
        database: Option<&str>,
    ) -> Result<PooledConnection, PoolError> {
        let (idle_opt, target_addr) = {
            let mut inner = self.inner.lock().await;
            match role {
                BackendRole::Leader => {
                    let addr = inner.leader_addr.clone().ok_or_else(|| {
                        PoolError::BackendUnavailable("No active Raft leader registered".into())
                    })?;
                    let conn = inner.idle_leaders.pop_front();
                    (conn, addr)
                }
                BackendRole::Standby => {
                    if !inner.standby_addrs.is_empty() {
                        let num_standbys = inner.standby_addrs.len();
                        let target_idx = inner.standby_rr_index % num_standbys;
                        inner.standby_rr_index = inner.standby_rr_index.wrapping_add(1);
                        let addr = inner.standby_addrs[target_idx].clone();

                        let conn = inner
                            .idle_standbys
                            .get_mut(&addr)
                            .and_then(|q| q.pop_front());

                        (conn, addr)
                    } else if let Some(addr) = inner.leader_addr.clone() {
                        // Fallback to leader if no standby is currently registered
                        (inner.idle_leaders.pop_front(), addr)
                    } else {
                        return Err(PoolError::BackendUnavailable(
                            "No database instances available".into(),
                        ));
                    }
                }
            }
        };

        if let Some(conn) = idle_opt {
            debug!(addr = %conn.addr, ?role, "Reusing idle pooled connection");
            Ok(conn)
        } else {
            PooledConnection::connect(&target_addr, role, user, database).await
        }
    }

    /// Acquires an idle connection or establishes a new one for the specified role.
    pub async fn acquire(&self, role: BackendRole) -> Result<PooledConnection, PoolError> {
        self.acquire_for(role, None, None).await
    }

    /// Acquires a connection, pausing and retrying if the cluster is undergoing failover.
    pub async fn acquire_with_retry(
        &self,
        role: BackendRole,
        config: &FailoverConfig,
        user: Option<&str>,
        database: Option<&str>,
    ) -> Result<PooledConnection, PoolError> {
        let start = Instant::now();
        let mut last_warn = Instant::now();
        let mut first = true;
        let mut rx = self.subscribe_topology();

        loop {
            match self.acquire_for(role, user, database).await {
                Ok(conn) => return Ok(conn),
                Err(err) => {
                    if start.elapsed() >= config.failover_timeout {
                        return Err(PoolError::FailoverTimeout(format!(
                            "Timed out after {:?} waiting for {:?}: {}",
                            start.elapsed(),
                            role,
                            err
                        )));
                    }

                    if first || last_warn.elapsed() >= Duration::from_secs(2) {
                        warn!(
                            ?role,
                            elapsed = ?start.elapsed(),
                            timeout = ?config.failover_timeout,
                            "Backend temporarily unavailable during failover; buffering client request"
                        );
                        last_warn = Instant::now();
                        first = false;
                    }

                    // Wait for either a topology change notification or retry interval tick
                    tokio::select! {
                        _ = tokio::time::sleep(config.retry_interval) => {}
                        res = rx.changed() => {
                            if res.is_err() {
                                tokio::time::sleep(config.retry_interval).await;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Releases a healthy backend connection back to the idle pool on transaction boundary.
    pub async fn release(&self, conn: PooledConnection) {
        let mut inner = self.inner.lock().await;
        match conn.role {
            BackendRole::Leader => {
                if Some(&conn.addr) == inner.leader_addr.as_ref()
                    && inner.idle_leaders.len() < inner.max_idle_per_node
                {
                    inner.idle_leaders.push_back(conn);
                }
            }
            BackendRole::Standby => {
                let max_idle = inner.max_idle_per_node;
                if inner.standby_addrs.contains(&conn.addr) {
                    let queue = inner.idle_standbys.entry(conn.addr.clone()).or_default();
                    if queue.len() < max_idle {
                        queue.push_back(conn);
                    }
                }
            }
        }
    }

    /// Returns the number of idle connections across leader and standbys.
    pub async fn idle_count(&self) -> (usize, usize) {
        let inner = self.inner.lock().await;
        let standbys = inner.idle_standbys.values().map(|q| q.len()).sum();
        (inner.idle_leaders.len(), standbys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_topology_and_failover_drain() {
        let pool = ConnectionPool::new(5);
        pool.update_topology(Some("127.0.0.1:5432".into()), vec!["127.0.0.1:5433".into()])
            .await;

        let (leaders, standbys) = pool.idle_count().await;
        assert_eq!(leaders, 0);
        assert_eq!(standbys, 0);

        // Failover: new leader promoted to 127.0.0.1:5434
        pool.update_topology(Some("127.0.0.1:5434".into()), vec![])
            .await;
        let inner = pool.inner.lock().await;
        assert_eq!(inner.leader_addr, Some("127.0.0.1:5434".into()));
        assert!(inner.idle_leaders.is_empty());
    }

    #[tokio::test]
    async fn test_standby_round_robin_selection() {
        let port1 = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let port2 = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };

        let pool = ConnectionPool::new(5);
        pool.update_topology(
            Some("127.0.0.1:5432".into()),
            vec![
                format!("127.0.0.1:{}", port1),
                format!("127.0.0.1:{}", port2),
            ],
        )
        .await;

        // Verify round-robin targeting across multiple standbys
        let err1 = pool.acquire_for(BackendRole::Standby, None, None).await;
        assert!(err1.is_err());
        {
            let inner = pool.inner.lock().await;
            assert_eq!(inner.standby_rr_index, 1);
        }

        let err2 = pool.acquire_for(BackendRole::Standby, None, None).await;
        assert!(err2.is_err());
        {
            let inner = pool.inner.lock().await;
            assert_eq!(inner.standby_rr_index, 2);
        }

        // When node 2 is down and removed from topology, remaining node 3 accepts reads
        pool.update_topology(
            Some("127.0.0.1:5432".into()),
            vec![format!("127.0.0.1:{}", port2)],
        )
        .await;

        {
            let inner = pool.inner.lock().await;
            assert_eq!(inner.standby_addrs.len(), 1);
            assert_eq!(inner.standby_rr_index, 0);
        }

        let err3 = pool.acquire_for(BackendRole::Standby, None, None).await;
        assert!(err3.is_err());
        {
            let inner = pool.inner.lock().await;
            assert_eq!(inner.standby_rr_index, 1);
        }
    }

    #[tokio::test]
    async fn test_failover_retry_timeout() {
        let pool = ConnectionPool::new(5);
        let config = FailoverConfig {
            failover_timeout: Duration::from_millis(150),
            retry_interval: Duration::from_millis(30),
        };

        let result = pool
            .acquire_with_retry(BackendRole::Leader, &config, None, None)
            .await;
        assert!(result.is_err());
        match result {
            Err(PoolError::FailoverTimeout(_)) => {}
            Err(e) => panic!("Expected FailoverTimeout, got error: {}", e),
            Ok(_) => panic!("Expected FailoverTimeout, but connection succeeded"),
        }
    }
}
