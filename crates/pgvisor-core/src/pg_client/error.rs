use thiserror::Error;

/// Errors returned by the shared PostgreSQL wire-protocol client.
#[derive(Debug, Error)]
pub enum PgClientError {
    #[error("I/O error communicating with PostgreSQL: {0}")]
    Io(#[from] std::io::Error),

    #[error("PostgreSQL backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("PostgreSQL query error: {0}")]
    SqlError(String),
}
