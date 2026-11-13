use std::time::Duration;
use thiserror::Error;

/// Errors returned by [`super::SqlSecurityGuard`] when validating SQL statements.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecurityError {
    #[error("Mutating SQL statement forbidden in dashboard read-only console: {0}")]
    MutationForbidden(String),

    #[error("Multi-statement SQL queries are forbidden: {0}")]
    MultiStatementForbidden(String),

    #[error("Empty SQL query")]
    EmptyQuery,

    #[error("Query execution timed out after {0:?}")]
    ExecutionTimeout(Duration),
}
