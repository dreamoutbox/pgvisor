use std::time::Instant;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::warn;

use super::error::PgClientError;
use super::frame::extract_backend_frame;
use crate::api::SqlQueryResult;
use crate::protocol::message::{BackendMessage, FrontendMessage};

/// Accumulates decoded PostgreSQL simple-query backend frames until `ReadyForQuery`.
pub struct SimpleQueryAccumulator {
    max_rows: usize,
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    command_tags: Vec<String>,
    error_msg: Option<String>,
    truncated: bool,
    finished: bool,
}

impl SimpleQueryAccumulator {
    /// Creates a new accumulator capping returned rows at `max_rows`.
    pub fn new(max_rows: usize) -> Self {
        Self {
            max_rows,
            columns: Vec::new(),
            rows: Vec::new(),
            command_tags: Vec::new(),
            error_msg: None,
            truncated: false,
            finished: false,
        }
    }

    /// Consumes all complete frames currently in `buf`.
    /// Returns `true` once `ReadyForQuery` has been observed.
    pub fn feed_buf(&mut self, buf: &mut BytesMut) -> bool {
        while let Some((_tag, mut frame)) = extract_backend_frame(buf) {
            match BackendMessage::decode(&mut frame) {
                Ok(Some(msg)) => match msg {
                    BackendMessage::RowDescription { columns: cols } => {
                        self.columns = cols;
                        self.rows.clear();
                    }
                    BackendMessage::DataRow { values } => {
                        if self.rows.len() < self.max_rows {
                            self.rows.push(
                                values
                                    .into_iter()
                                    .map(|v| v.unwrap_or_else(|| "NULL".to_string()))
                                    .collect(),
                            );
                        } else {
                            self.truncated = true;
                        }
                    }
                    BackendMessage::CommandComplete { tag } => {
                        self.command_tags.push(tag);
                    }
                    BackendMessage::ErrorResponse { message } => {
                        self.error_msg = Some(message);
                    }
                    BackendMessage::ReadyForQuery { .. } => {
                        self.finished = true;
                        break;
                    }
                    _ => {}
                },
                Ok(None) => {}
                Err(e) => {
                    warn!(?e, "Error decoding backend message in SimpleQueryAccumulator");
                }
            }
        }
        self.finished
    }

    /// Consumes the accumulator and produces the [`SqlQueryResult`] or SQL error.
    pub fn finish(mut self, execution_time_ms: u64) -> Result<SqlQueryResult, PgClientError> {
        if let Some(err) = self.error_msg {
            return Err(PgClientError::SqlError(err));
        }

        // For DDL or DML statements without returned rows (e.g. CREATE, DROP, INSERT),
        // synthesize status rows from the command completion tags.
        if self.columns.is_empty() && !self.command_tags.is_empty() {
            self.columns = vec!["status".into()];
            self.rows = self
                .command_tags
                .into_iter()
                .map(|tag| vec![tag])
                .collect();
        }

        let row_count = self.rows.len();

        Ok(SqlQueryResult {
            columns: self.columns,
            rows: self.rows,
            execution_time_ms,
            row_count,
            truncated: self.truncated,
        })
    }
}

/// Executes a simple query on an already-authenticated PostgreSQL stream.
///
/// # Arguments
/// * `stream` - Connected and authenticated async read/write stream.
/// * `sql` - SQL query string to execute.
/// * `max_rows` - Maximum number of rows to retain before truncating.
pub async fn execute_simple_query_on_stream<S>(
    stream: &mut S,
    sql: &str,
    max_rows: usize,
) -> Result<SqlQueryResult, PgClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let start = Instant::now();

    let mut query_buf = BytesMut::new();
    FrontendMessage::Query(sql.to_string()).encode(&mut query_buf);
    stream.write_all(&query_buf).await?;

    let mut read_buf = BytesMut::with_capacity(4096);
    let mut acc = SimpleQueryAccumulator::new(max_rows);

    loop {
        let n = stream.read_buf(&mut read_buf).await?;
        if n == 0 {
            return Err(PgClientError::BackendUnavailable(
                "Backend connection closed unexpectedly during query".into(),
            ));
        }

        if acc.feed_buf(&mut read_buf) {
            break;
        }
    }

    let elapsed = start.elapsed().as_millis() as u64;
    acc.finish(elapsed)
}
