use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
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

impl PooledConnection {
    pub async fn connect(addr: &str, role: BackendRole) -> Result<Self, PoolError> {
        debug!(addr, ?role, "Opening new backend connection");
        let stream = TcpStream::connect(addr).await?;
        Ok(Self {
            stream,
            role,
            addr: addr.to_string(),
            created_at: Instant::now(),
        })
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
    idle_leaders: VecDeque<PooledConnection>,
    idle_standbys: VecDeque<PooledConnection>,
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
                idle_leaders: VecDeque::new(),
                idle_standbys: VecDeque::new(),
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

        inner.standby_addrs = standbys;
        inner.version = inner.version.wrapping_add(1);
        let _ = self.topology_notifier.send(inner.version);
    }

    /// Returns a receiver for topology updates.
    pub fn subscribe_topology(&self) -> watch::Receiver<u64> {
        self.topology_notifier.subscribe()
    }

    /// Acquires an idle connection or establishes a new one for the specified role.
    pub async fn acquire(&self, role: BackendRole) -> Result<PooledConnection, PoolError> {
        let (idle_opt, target_addr) = {
            let mut inner = self.inner.lock().await;
            match role {
                BackendRole::Leader => {
                    let addr = inner
                        .leader_addr
                        .clone()
                        .ok_or_else(|| PoolError::BackendUnavailable("No active Raft leader registered".into()))?;
                    let conn = inner.idle_leaders.pop_front();
                    (conn, addr)
                }
                BackendRole::Standby => {
                    if let Some(conn) = inner.idle_standbys.pop_front() {
                        let addr = conn.addr.clone();
                        (Some(conn), addr)
                    } else if let Some(addr) = inner.standby_addrs.first().cloned() {
                        (None, addr)
                    } else if let Some(addr) = inner.leader_addr.clone() {
                        // Fallback to leader if no standby is currently registered
                        (inner.idle_leaders.pop_front(), addr)
                    } else {
                        return Err(PoolError::BackendUnavailable("No database instances available".into()));
                    }
                }
            }
        };

        if let Some(conn) = idle_opt {
            debug!(addr = %conn.addr, ?role, "Reusing idle pooled connection");
            Ok(conn)
        } else {
            PooledConnection::connect(&target_addr, role).await
        }
    }

    /// Acquires a connection, pausing and retrying if the cluster is undergoing failover.
    pub async fn acquire_with_retry(
        &self,
        role: BackendRole,
        config: &FailoverConfig,
    ) -> Result<PooledConnection, PoolError> {
        let start = Instant::now();
        let mut rx = self.subscribe_topology();

        loop {
            match self.acquire(role).await {
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

                    warn!(
                        ?role,
                        elapsed = ?start.elapsed(),
                        timeout = ?config.failover_timeout,
                        "Backend temporarily unavailable during failover; buffering client request"
                    );

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
                if inner.idle_standbys.len() < inner.max_idle_per_node {
                    inner.idle_standbys.push_back(conn);
                }
            }
        }
    }

    /// Returns the number of idle connections across leader and standbys.
    pub async fn idle_count(&self) -> (usize, usize) {
        let inner = self.inner.lock().await;
        (inner.idle_leaders.len(), inner.idle_standbys.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_topology_and_failover_drain() {
        let pool = ConnectionPool::new(5);
        pool.update_topology(Some("127.0.0.1:5432".into()), vec!["127.0.0.1:5433".into()]).await;

        let (leaders, standbys) = pool.idle_count().await;
        assert_eq!(leaders, 0);
        assert_eq!(standbys, 0);

        // Failover: new leader promoted to 127.0.0.1:5434
        pool.update_topology(Some("127.0.0.1:5434".into()), vec![]).await;
        let inner = pool.inner.lock().await;
        assert_eq!(inner.leader_addr, Some("127.0.0.1:5434".into()));
        assert!(inner.idle_leaders.is_empty());
    }

    #[tokio::test]
    async fn test_failover_retry_timeout() {
        let pool = ConnectionPool::new(5);
        let config = FailoverConfig {
            failover_timeout: Duration::from_millis(150),
            retry_interval: Duration::from_millis(30),
        };

        let result = pool.acquire_with_retry(BackendRole::Leader, &config).await;
        assert!(result.is_err());
        match result {
            Err(PoolError::FailoverTimeout(_)) => {}
            other => panic!("Expected FailoverTimeout, got {:?}", other),
        }
    }
}
