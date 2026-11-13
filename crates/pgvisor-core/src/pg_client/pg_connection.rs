use std::collections::HashMap;
use std::sync::Arc;

use bytes::BytesMut;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::debug;

use super::backend_stream::BackendStream;
use super::error::PgClientError;
use super::simple_query::execute_simple_query_on_stream;
use crate::api::SqlQueryResult;
use crate::protocol::message::{BackendMessage, InitialClientMessage, StartupMessage};

/// Direct PostgreSQL wire-protocol connection for running simple queries.
pub struct PgConnection {
    pub stream: BackendStream,
    pub addr: String,
    pub database: String,
}

impl PgConnection {
    /// Connects to a PostgreSQL instance without TLS and completes the startup handshake.
    ///
    /// # Arguments
    /// * `addr` - Target `host:port` address.
    /// * `user` - Optional PostgreSQL username (defaults to `"postgres"`).
    /// * `database` - Optional database name (defaults to `"postgres"`).
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example() -> Result<(), pgvisor_core::pg_client::PgClientError> {
    /// use pgvisor_core::pg_client::PgConnection;
    /// let mut conn = PgConnection::connect("127.0.0.1:5432", Some("postgres"), Some("postgres")).await?;
    /// let res = conn.execute_simple_query("SELECT 1", 10).await?;
    /// assert_eq!(res.row_count, 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(
        addr: &str,
        user: Option<&str>,
        database: Option<&str>,
    ) -> Result<Self, PgClientError> {
        Self::connect_with_tls(addr, user, database, None).await
    }

    /// Connects to a PostgreSQL instance with optional TLS encryption and completes startup.
    pub async fn connect_with_tls(
        addr: &str,
        user: Option<&str>,
        database: Option<&str>,
        tls_connector: Option<Arc<TlsConnector>>,
    ) -> Result<Self, PgClientError> {
        debug!(
            addr,
            has_tls = tls_connector.is_some(),
            "Opening new PostgreSQL client connection"
        );
        let mut tcp_stream = TcpStream::connect(addr).await?;

        let mut stream = if let Some(connector) = tls_connector {
            let mut ssl_req = [0u8; 8];
            ssl_req[0..4].copy_from_slice(&8i32.to_be_bytes());
            ssl_req[4..8].copy_from_slice(&(crate::protocol::SSL_REQUEST_CODE).to_be_bytes());
            tcp_stream.write_all(&ssl_req).await?;

            let mut resp = [0u8; 1];
            tcp_stream.read_exact(&mut resp).await?;
            if resp[0] == b'S' {
                let host = addr.split(':').next().unwrap_or(addr);
                let server_name = ServerName::try_from(host.to_string()).map_err(|e| {
                    PgClientError::BackendUnavailable(format!("Invalid DNS name {}: {}", host, e))
                })?;
                let tls_stream = connector
                    .connect(server_name, tcp_stream)
                    .await
                    .map_err(|e| {
                        PgClientError::BackendUnavailable(format!(
                            "TLS handshake failed with backend {}: {}",
                            addr, e
                        ))
                    })?;
                BackendStream::Tls(Box::new(tls_stream))
            } else {
                return Err(PgClientError::BackendUnavailable(format!(
                    "Backend at {} does not support SSL (received '{}')",
                    addr, resp[0] as char
                )));
            }
        } else {
            BackendStream::Plain(tcp_stream)
        };

        let mut startup_buf = BytesMut::new();
        let mut params = HashMap::new();
        let db_name = database.unwrap_or("postgres").to_string();
        params.insert("user".to_string(), user.unwrap_or("postgres").to_string());
        params.insert("database".to_string(), db_name.clone());
        params.insert("client_encoding".to_string(), "UTF8".to_string());

        let startup = StartupMessage {
            protocol_version: 196608,
            parameters: params,
        };
        InitialClientMessage::encode_startup(&startup, &mut startup_buf);
        stream.write_all(&startup_buf).await?;

        let mut read_buf = BytesMut::with_capacity(4096);
        loop {
            let n = stream.read_buf(&mut read_buf).await?;
            if n == 0 {
                return Err(PgClientError::BackendUnavailable(format!(
                    "Backend at {} unexpectedly closed connection during startup handshake",
                    addr
                )));
            }

            while let Some(msg) = BackendMessage::decode(&mut read_buf)? {
                match msg {
                    BackendMessage::ReadyForQuery { .. } => {
                        debug!(addr, "PostgreSQL client startup handshake completed");
                        return Ok(Self {
                            stream,
                            addr: addr.to_string(),
                            database: db_name,
                        });
                    }
                    BackendMessage::ErrorResponse { message } => {
                        return Err(PgClientError::BackendUnavailable(format!(
                            "Backend at {} rejected startup handshake: {}",
                            addr, message
                        )));
                    }
                    _ => {}
                }
            }
        }
    }

    /// Executes a simple query on this connection.
    pub async fn execute_simple_query(
        &mut self,
        sql: &str,
        max_rows: usize,
    ) -> Result<SqlQueryResult, PgClientError> {
        execute_simple_query_on_stream(&mut self.stream, sql, max_rows).await
    }

    /// Connects, runs one simple query, and closes the connection.
    pub async fn query_simple(
        addr: &str,
        user: Option<&str>,
        database: Option<&str>,
        sql: &str,
        max_rows: usize,
    ) -> Result<SqlQueryResult, PgClientError> {
        let mut conn = Self::connect(addr, user, database).await?;
        conn.execute_simple_query(sql, max_rows).await
    }
}
