use bytes::BytesMut;
use pgvisor_core::protocol::message::{
    BackendMessage, FrontendMessage, InitialClientMessage, StartupMessage, TransactionStatus,
};
use pgvisor_core::protocol::tracker::TransactionTracker;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::pool::{BackendRole, ConnectionPool, FailoverConfig, PooledConnection};

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Pool error: {0}")]
    Pool(#[from] crate::pool::PoolError),

    #[error("Protocol error: {0}")]
    Protocol(String),
}

/// Manages a single client frontend session with transaction-level backend pooling.
pub struct ClientSession {
    client_stream: TcpStream,
    pool: ConnectionPool,
    tracker: TransactionTracker,
    active_backend: Option<PooledConnection>,
    startup_params: Option<StartupMessage>,
    failover_config: FailoverConfig,
}

impl ClientSession {
    pub fn new(client_stream: TcpStream, pool: ConnectionPool) -> Self {
        Self {
            client_stream,
            pool,
            tracker: TransactionTracker::new(),
            active_backend: None,
            startup_params: None,
            failover_config: FailoverConfig::default(),
        }
    }

    /// Sets custom failover buffering parameters.
    pub fn with_failover_config(mut self, config: FailoverConfig) -> Self {
        self.failover_config = config;
        self
    }

    /// Returns the initial startup parameters provided by the client.
    pub fn startup_params(&self) -> Option<&StartupMessage> {
        self.startup_params.as_ref()
    }

    /// Drives the client session through handshake and query execution loop.
    pub async fn run(&mut self) -> Result<(), SessionError> {
        let mut buf = BytesMut::with_capacity(4096);

        // 1. Initial Handshake phase
        loop {
            if self.client_stream.read_buf(&mut buf).await? == 0 {
                return Ok(()); // Client disconnected
            }

            if let Some(initial_msg) = InitialClientMessage::decode(&mut buf)? {
                match initial_msg {
                    InitialClientMessage::SslRequest => {
                        debug!("Client requested SSL, responding with 'N' (SSL unencrypted)");
                        self.client_stream.write_all(b"N").await?;
                    }
                    InitialClientMessage::Startup(startup) => {
                        info!(
                            user = ?startup.user(),
                            database = ?startup.database(),
                            "Client handshake initialized"
                        );
                        self.startup_params = Some(startup);
                        // Complete handshake by sending AuthenticationOk + ReadyForQuery
                        let mut resp = BytesMut::new();
                        BackendMessage::AuthenticationOk.encode(&mut resp);
                        BackendMessage::ReadyForQuery {
                            status: TransactionStatus::Idle,
                        }
                        .encode(&mut resp);
                        self.client_stream.write_all(&resp).await?;
                        break;
                    }
                    InitialClientMessage::CancelRequest { process_id, .. } => {
                        debug!(process_id, "Cancel request received, closing session");
                        return Ok(());
                    }
                }
            }
        }

        // 2. Command processing loop
        loop {
            if self.client_stream.read_buf(&mut buf).await? == 0 {
                debug!("Client closed connection");
                break;
            }

            while let Some(msg) = FrontendMessage::decode(&mut buf)? {
                match msg {
                    FrontendMessage::Query(sql) => {
                        self.handle_query(&sql).await?;
                    }
                    FrontendMessage::Terminate => {
                        debug!("Client sent Terminate ('X')");
                        return Ok(());
                    }
                    FrontendMessage::Sync | FrontendMessage::Flush => {
                        // Forward synchronization packets if an active backend exists
                        if let Some(backend) = self.active_backend.as_mut() {
                            let mut out = BytesMut::new();
                            msg.encode(&mut out);
                            backend.stream.write_all(&out).await?;
                        }
                    }
                    FrontendMessage::Raw { tag, payload } => {
                        // Transparently forward extended query protocol messages
                        if let Some(backend) = self.active_backend.as_mut() {
                            let mut out = BytesMut::new();
                            FrontendMessage::Raw { tag, payload }.encode(&mut out);
                            backend.stream.write_all(&out).await?;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Return any active backend connection on session drop
        if let Some(backend) = self.active_backend.take() {
            self.pool.release(backend).await;
        }

        Ok(())
    }

    /// Handles a simple query ('Q') with failover buffering, read/write splitting, and reconnect.
    async fn handle_query(&mut self, sql: &str) -> Result<(), SessionError> {
        let requires_leader = self.tracker.requires_leader(sql);
        let role = if requires_leader {
            BackendRole::Leader
        } else {
            BackendRole::Standby
        };

        // If no backend is held, acquire one from pool with failover buffering
        if self.active_backend.is_none() {
            match self.pool.acquire_with_retry(role, &self.failover_config).await {
                Ok(backend) => {
                    self.active_backend = Some(backend);
                }
                Err(err) => {
                    warn!(?err, "Failed to acquire backend within failover window; notifying client");
                    let mut err_resp = BytesMut::new();
                    BackendMessage::ErrorResponse {
                        message: format!("PgVisor failover timeout: {}", err),
                    }
                    .encode(&mut err_resp);
                    BackendMessage::ReadyForQuery {
                        status: TransactionStatus::Idle,
                    }
                    .encode(&mut err_resp);
                    self.client_stream.write_all(&err_resp).await?;
                    return Ok(());
                }
            }
        }

        let mut backend = match self.active_backend.take() {
            Some(b) => b,
            None => return Err(SessionError::Protocol("No backend assigned".into())),
        };

        // Buffer the outbound query message
        let mut forward_buf = BytesMut::new();
        FrontendMessage::Query(sql.to_string()).encode(&mut forward_buf);

        // Forward query to the backend
        if let Err(e) = backend.stream.write_all(&forward_buf).await {
            warn!(?e, "Failed to write query to backend; attempting transparent failover re-acquire");
            // Discard broken backend and attempt transparent failover retry
            match self.pool.acquire_with_retry(role, &self.failover_config).await {
                Ok(mut new_backend) => {
                    new_backend.stream.write_all(&forward_buf).await?;
                    backend = new_backend;
                }
                Err(err) => {
                    let mut err_resp = BytesMut::new();
                    BackendMessage::ErrorResponse {
                        message: format!("PgVisor backend write failed during failover: {}", err),
                    }
                    .encode(&mut err_resp);
                    BackendMessage::ReadyForQuery {
                        status: TransactionStatus::Idle,
                    }
                    .encode(&mut err_resp);
                    self.client_stream.write_all(&err_resp).await?;
                    return Ok(());
                }
            }
        }

        // Stream backend responses back to client and watch for ReadyForQuery ('Z')
        let mut backend_read_buf = BytesMut::with_capacity(4096);
        let mut should_release = false;
        let mut client_bytes_written = 0usize;

        loop {
            let n = match backend.stream.read_buf(&mut backend_read_buf).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(?e, "Backend socket error during query read");
                    0
                }
            };

            if n == 0 {
                // If backend terminated mid-query:
                // If 0 bytes were sent to client, we can transparently retry query on new leader!
                if client_bytes_written == 0 {
                    info!("Backend terminated before sending response; retrying query on newly promoted leader");
                    match self.pool.acquire_with_retry(role, &self.failover_config).await {
                        Ok(mut new_backend) => {
                            new_backend.stream.write_all(&forward_buf).await?;
                            backend = new_backend;
                            backend_read_buf.clear();
                            continue;
                        }
                        Err(err) => {
                            let mut err_resp = BytesMut::new();
                            BackendMessage::ErrorResponse {
                                message: format!("PgVisor failover retry failed: {}", err),
                            }
                            .encode(&mut err_resp);
                            BackendMessage::ReadyForQuery {
                                status: TransactionStatus::Idle,
                            }
                            .encode(&mut err_resp);
                            self.client_stream.write_all(&err_resp).await?;
                            return Ok(());
                        }
                    }
                } else {
                    // Bytes were already sent to client, cannot transparently retry
                    warn!("Backend terminated after partial response streamed to client; sending ErrorResponse");
                    let mut err_resp = BytesMut::new();
                    BackendMessage::ErrorResponse {
                        message: "PgVisor: backend connection lost during failover mid-response".into(),
                    }
                    .encode(&mut err_resp);
                    BackendMessage::ReadyForQuery {
                        status: TransactionStatus::Idle,
                    }
                    .encode(&mut err_resp);
                    self.client_stream.write_all(&err_resp).await?;
                    return Ok(());
                }
            }

            // Inspect packets to update transaction status
            let mut inspect_buf = backend_read_buf.clone();
            while let Some(msg) = BackendMessage::decode(&mut inspect_buf)? {
                if let BackendMessage::ReadyForQuery { status } = msg {
                    self.tracker.on_ready_for_query(status);

                    if status == TransactionStatus::Idle {
                        should_release = true;
                    }
                }
            }

            // Write raw bytes directly to client
            client_bytes_written += backend_read_buf.len();
            self.client_stream.write_all(&backend_read_buf).await?;
            backend_read_buf.clear();

            if self.tracker.is_idle() || should_release {
                break;
            }
        }

        if should_release {
            debug!(addr = %backend.addr, "Transaction complete, returning backend to pool");
            self.pool.release(backend).await;
        } else {
            self.active_backend = Some(backend);
        }

        Ok(())
    }
}
