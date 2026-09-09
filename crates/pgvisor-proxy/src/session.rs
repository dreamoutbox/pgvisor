use bytes::BytesMut;
use pgvisor_core::audit::{AuditEventKind, AuditLog};
use pgvisor_core::protocol::message::{
    BackendMessage, FrontendMessage, InitialClientMessage, StartupMessage, TransactionStatus,
};
use pgvisor_core::protocol::tracker::TransactionTracker;
use std::sync::Arc;
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
    audit_log: Option<Arc<AuditLog>>,
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
            audit_log: None,
        }
    }

    /// Sets the central audit log store for recording client operations.
    pub fn with_audit_log(mut self, audit_log: Arc<AuditLog>) -> Self {
        self.audit_log = Some(audit_log);
        self
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
                        // Complete handshake by sending AuthenticationOk + ParameterStatus + BackendKeyData + ReadyForQuery
                        let mut resp = BytesMut::new();
                        BackendMessage::AuthenticationOk.encode(&mut resp);

                        // Standard parameter statuses expected by Postgres clients
                        BackendMessage::ParameterStatus {
                            name: "server_version".into(),
                            value: "18.6".into(),
                        }
                        .encode(&mut resp);
                        BackendMessage::ParameterStatus {
                            name: "server_encoding".into(),
                            value: "UTF8".into(),
                        }
                        .encode(&mut resp);
                        BackendMessage::ParameterStatus {
                            name: "client_encoding".into(),
                            value: "UTF8".into(),
                        }
                        .encode(&mut resp);
                        BackendMessage::ParameterStatus {
                            name: "standard_conforming_strings".into(),
                            value: "on".into(),
                        }
                        .encode(&mut resp);
                        BackendMessage::ParameterStatus {
                            name: "TimeZone".into(),
                            value: "UTC".into(),
                        }
                        .encode(&mut resp);

                        // BackendKeyData providing cancellation keys for psql (PQcancel / Ctrl+C)
                        BackendMessage::BackendKeyData {
                            process_id: std::process::id(),
                            secret_key: 12345678,
                        }
                        .encode(&mut resp);

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

        let user = self.startup_params.as_ref().and_then(|s| s.user());
        let database = self.startup_params.as_ref().and_then(|s| s.database());

        // Audit dangerous SQL statements (DROP TABLE, TRUNCATE, and DELETE)
        if let Some(audit) = self.audit_log.as_ref() {
            let clean = sql
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.starts_with("--") && !l.starts_with("/*"))
                .collect::<Vec<_>>()
                .join(" ");
            let upper = clean.trim().to_uppercase();

            let is_drop = upper.starts_with("DROP TABLE")
                || upper.starts_with("DROP DATABASE")
                || upper.starts_with("DROP SCHEMA");
            let is_truncate = upper.starts_with("TRUNCATE");
            let is_delete = if upper.starts_with("DELETE") {
                if std::env::var("PGVISOR_AUDIT_DELETE").is_ok() {
                    true
                } else {
                    !upper.contains("WHERE")
                }
            } else {
                false
            };

            if is_drop || is_truncate || is_delete {
                let now = chrono::Utc::now();
                let pitr_candidate = (now - chrono::Duration::seconds(1))
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string();
                let u = user.unwrap_or("unknown");
                let db = database.unwrap_or("unknown");
                let snippet = if sql.len() > 300 {
                    format!("{}...", &sql[..300])
                } else {
                    sql.to_string()
                };
                let detail = format!("Dangerous SQL executed by '{}' on '{}': {}", u, db, snippet);

                audit
                    .append(
                        AuditEventKind::DangerousSql,
                        None,
                        None,
                        detail,
                        Some(pitr_candidate),
                    )
                    .await;
            }

            let is_user_permission = upper.starts_with("CREATE ROLE")
                || upper.starts_with("CREATE USER")
                || upper.starts_with("DROP ROLE")
                || upper.starts_with("DROP USER")
                || upper.starts_with("ALTER ROLE")
                || upper.starts_with("ALTER USER")
                || upper.starts_with("GRANT ")
                || upper.starts_with("REVOKE ");

            if is_user_permission {
                let u = user.unwrap_or("unknown");
                let db = database.unwrap_or("unknown");
                let snippet = if sql.len() > 300 {
                    format!("{}...", &sql[..300])
                } else {
                    sql.to_string()
                };
                let detail = format!("User/permission SQL executed by '{}' on '{}': {}", u, db, snippet);
                audit
                    .append(
                        AuditEventKind::UserPermission,
                        None,
                        None,
                        detail,
                        None,
                    )
                    .await;
            }
        }

        // If no backend is held, acquire one from pool with failover buffering
        if self.active_backend.is_none() {
            match self
                .pool
                .acquire_with_retry(role, &self.failover_config, user, database)
                .await
            {
                Ok(backend) => {
                    self.active_backend = Some(backend);
                }
                Err(err) => {
                    warn!(
                        ?err,
                        "Failed to acquire backend within failover window; notifying client"
                    );
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
            warn!(
                ?e,
                "Failed to write query to backend; attempting transparent failover re-acquire"
            );
            // Discard broken backend and attempt transparent failover retry
            match self
                .pool
                .acquire_with_retry(role, &self.failover_config, user, database)
                .await
            {
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
        let mut ready_for_query_received = false;
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
                    match self
                        .pool
                        .acquire_with_retry(role, &self.failover_config, user, database)
                        .await
                    {
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
                        message: "PgVisor: backend connection lost during failover mid-response"
                            .into(),
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

            // Frame and forward complete backend messages
            while let Some((tag, frame)) = extract_backend_frame(&mut backend_read_buf) {
                if tag == b'Z' {
                    ready_for_query_received = true;
                    let status_byte = if frame.len() >= 6 { frame[5] } else { b'I' };
                    let status =
                        TransactionStatus::try_from(status_byte).unwrap_or(TransactionStatus::Idle);
                    self.tracker.on_ready_for_query(status);

                    if status == TransactionStatus::Idle {
                        should_release = true;
                    }
                }

                client_bytes_written += frame.len();
                self.client_stream.write_all(&frame).await?;

                if ready_for_query_received {
                    break;
                }
            }

            if ready_for_query_received {
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

/// Extracts a single PostgreSQL wire protocol frame from `buf` if complete.
/// A complete frame consists of 1 byte tag + 4 bytes length + (length - 4) payload bytes.
pub fn extract_backend_frame(buf: &mut BytesMut) -> Option<(u8, BytesMut)> {
    if buf.len() < 5 {
        return None;
    }
    let tag = buf[0];
    let len = i32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 1 + len {
        return None;
    }
    Some((tag, buf.split_to(1 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_backend_frame_partial_and_complete() {
        let mut buf = BytesMut::new();

        // 1. Partial header (< 5 bytes)
        buf.extend_from_slice(&[b'C', 0, 0]);
        assert_eq!(extract_backend_frame(&mut buf), None);
        assert_eq!(buf.len(), 3); // Unconsumed

        // 2. Complete header but partial body
        // CommandComplete 'C', len = 9 (4 header + 5 body: "DROP\0")
        buf.extend_from_slice(&[0, 9, b'D', b'R']);
        assert_eq!(extract_backend_frame(&mut buf), None);

        // 3. Complete body
        buf.extend_from_slice(&[b'O', b'P', 0]);
        let extracted = extract_backend_frame(&mut buf);
        assert!(extracted.is_some());
        let (tag, frame) = extracted.unwrap();
        assert_eq!(tag, b'C');
        assert_eq!(frame.len(), 10);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_extract_multiple_frames_with_ready_for_query() {
        let mut buf = BytesMut::new();

        // Frame 1: NoticeResponse 'N', len = 8 (4 + 4 body)
        buf.extend_from_slice(&[b'N', 0, 0, 0, 8, b'W', b'A', b'R', 0]);
        // Frame 2: ReadyForQuery 'Z', len = 5 (4 + 1 body: 'I')
        buf.extend_from_slice(&[b'Z', 0, 0, 0, 5, b'I']);

        let (tag1, frame1) = extract_backend_frame(&mut buf).unwrap();
        assert_eq!(tag1, b'N');
        assert_eq!(frame1.len(), 9);

        let (tag2, frame2) = extract_backend_frame(&mut buf).unwrap();
        assert_eq!(tag2, b'Z');
        assert_eq!(frame2.len(), 6);
        assert_eq!(frame2[5], b'I');

        assert!(extract_backend_frame(&mut buf).is_none());
    }
}
