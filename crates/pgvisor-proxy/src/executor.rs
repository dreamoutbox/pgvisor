use std::time::Instant;

use bytes::BytesMut;
use pgvisor_core::protocol::message::{BackendMessage, FrontendMessage};
use pgvisor_dashboard::handlers::SqlExecutor;
use pgvisor_dashboard::models::SqlQueryResult;
use pgvisor_dashboard::security::SqlSecurityGuard;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

use crate::pool::{BackendRole, ConnectionPool, FailoverConfig};
use crate::session::extract_backend_frame;

/// Determines if a SQL query requires the Raft Leader connection (mutations, DDL, multi-statements, transactions).
fn is_leader_required(sql: &str) -> bool {
    let cleaned = SqlSecurityGuard::strip_comments(sql);
    let trimmed = cleaned.trim();
    let without_trailing_semicolon = trimmed.trim_end_matches(';').trim();

    // Multi-statement script separated by semicolons must route to Leader
    if without_trailing_semicolon.contains(';') {
        return true;
    }

    let upper = without_trailing_semicolon.to_uppercase();
    let first_word = upper.split_whitespace().next().unwrap_or("");

    match first_word {
        "SELECT" => {
            // Check for SELECT INTO (which mutates database by creating a new table)
            upper.contains("INTO ")
        }
        "SHOW" | "EXPLAIN" => false,
        _ => true,
    }
}

/// SQL query executor using the proxy's connection pool to query the Postgres cluster.
pub struct ProxySqlExecutor {
    pool: ConnectionPool,
    failover_config: FailoverConfig,
}

impl ProxySqlExecutor {
    pub fn new(pool: ConnectionPool) -> Self {
        Self {
            pool,
            failover_config: FailoverConfig::default(),
        }
    }
}

#[async_trait::async_trait]
impl SqlExecutor for ProxySqlExecutor {
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<SqlQueryResult, String> {
        let start = Instant::now();
        let role = if is_leader_required(sql) {
            BackendRole::Leader
        } else {
            BackendRole::Standby
        };

        // Acquire a connection from the pool for the required role
        let mut backend = self
            .pool
            .acquire_with_retry(role, &self.failover_config, None, None)
            .await
            .map_err(|e| format!("Failed to acquire backend connection: {}", e))?;

        // Format and send Query message
        let mut query_buf = BytesMut::new();
        FrontendMessage::Query(sql.to_string()).encode(&mut query_buf);

        if let Err(e) = backend.stream.write_all(&query_buf).await {
            return Err(format!("Failed to write SQL query to backend: {}", e));
        }

        let mut read_buf = BytesMut::with_capacity(4096);
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut command_tags = Vec::new();
        let mut error_msg: Option<String> = None;
        let mut truncated = false;

        loop {
            let n = backend
                .stream
                .read_buf(&mut read_buf)
                .await
                .map_err(|e| format!("Error reading backend response: {}", e))?;

            if n == 0 {
                return Err("Backend connection closed unexpectedly during query".into());
            }

            let mut finished = false;
            while let Some((_tag, mut frame)) = extract_backend_frame(&mut read_buf) {
                match BackendMessage::decode(&mut frame) {
                    Ok(Some(msg)) => match msg {
                        BackendMessage::RowDescription { columns: cols } => {
                            columns = cols;
                            rows.clear();
                        }
                        BackendMessage::DataRow { values } => {
                            if rows.len() < max_rows {
                                rows.push(
                                    values
                                        .into_iter()
                                        .map(|v| v.unwrap_or_else(|| "NULL".to_string()))
                                        .collect(),
                                );
                            } else {
                                truncated = true;
                            }
                        }
                        BackendMessage::CommandComplete { tag } => {
                            command_tags.push(tag);
                        }
                        BackendMessage::ErrorResponse { message } => {
                            error_msg = Some(message);
                        }
                        BackendMessage::ReadyForQuery { .. } => {
                            finished = true;
                            break;
                        }
                        _ => {}
                    },
                    Ok(None) => {}
                    Err(e) => {
                        warn!(?e, "Error decoding backend message in ProxySqlExecutor");
                    }
                }
            }

            if finished {
                break;
            }
        }

        // Return connection back to the idle pool
        self.pool.release(backend).await;

        if let Some(err) = error_msg {
            return Err(err);
        }

        // For DDL or DML statements without returned rows (e.g. CREATE, DROP, INSERT),
        // synthesize status rows from the command completion tags.
        if columns.is_empty() && !command_tags.is_empty() {
            columns = vec!["status".into()];
            rows = command_tags.into_iter().map(|tag| vec![tag]).collect();
        }

        let row_count = rows.len();
        let elapsed = start.elapsed().as_millis() as u64;

        Ok(SqlQueryResult {
            columns,
            rows,
            execution_time_ms: elapsed,
            row_count,
            truncated,
        })
    }
}
