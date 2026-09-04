use bytes::BytesMut;
use pgvisor_core::protocol::message::{
    BackendMessage, FrontendMessage, InitialClientMessage, StartupMessage, TransactionStatus,
};
use pgvisor_core::protocol::tracker::TransactionTracker;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::pool::{BackendRole, ConnectionPool, PooledConnection};

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
}

impl ClientSession {
    pub fn new(client_stream: TcpStream, pool: ConnectionPool) -> Self {
        Self {
            client_stream,
            pool,
            tracker: TransactionTracker::new(),
            active_backend: None,
            startup_params: None,
        }
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

    /// Handles a simple query ('Q') with read/write splitting and transaction boundary release.
    async fn handle_query(&mut self, sql: &str) -> Result<(), SessionError> {
        let requires_leader = self.tracker.requires_leader(sql);
        let role = if requires_leader {
            BackendRole::Leader
        } else {
            BackendRole::Standby
        };

        // If no active backend connection is held for this transaction, acquire one from the pool.
        if self.active_backend.is_none() {
            let backend = self.pool.acquire(role).await?;
            self.active_backend = Some(backend);
        }

        let mut backend = self
            .active_backend
            .take()
            .ok_or_else(|| SessionError::Protocol("No backend assigned".into()))?;

        // Forward query to the borrowed backend
        let mut forward_buf = BytesMut::new();
        FrontendMessage::Query(sql.to_string()).encode(&mut forward_buf);
        backend.stream.write_all(&forward_buf).await?;

        // Stream backend responses back to client and watch for ReadyForQuery ('Z')
        let mut backend_read_buf = BytesMut::with_capacity(4096);
        let mut should_release = false;

        loop {
            let n = backend.stream.read_buf(&mut backend_read_buf).await?;
            if n == 0 {
                return Err(SessionError::Protocol("Backend unexpectedly closed connection".into()));
            }

            // Inspect packets to update transaction status
            let mut inspect_buf = backend_read_buf.clone();
            while let Some(msg) = BackendMessage::decode(&mut inspect_buf)? {
                if let BackendMessage::ReadyForQuery { status } = msg {
                    self.tracker.on_ready_for_query(status);

                    // In transaction pooling: if transaction is committed/idle, release backend back to pool!
                    if status == TransactionStatus::Idle {
                        should_release = true;
                    }
                }
            }

            // Write raw bytes directly to client
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
