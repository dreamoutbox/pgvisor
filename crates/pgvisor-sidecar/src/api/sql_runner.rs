use pgvisor_core::api::SqlQueryResult;
use pgvisor_core::pg_client::PgConnection;

use super::error::ApiError;

/// Executes SQL queries against PostgreSQL on behalf of a specific authenticated user.
#[async_trait::async_trait]
pub trait ApiSqlRunner: Send + Sync {
    async fn execute_as_user(
        &self,
        username: &str,
        database: Option<&str>,
        sql: &str,
        max_rows: usize,
    ) -> Result<SqlQueryResult, ApiError>;
}

/// Live [`ApiSqlRunner`] implementation backed by `PgConnection::query_simple`.
#[derive(Debug, Clone)]
pub struct LivePgSqlRunner {
    pg_addr: String,
}

impl LivePgSqlRunner {
    /// Creates a new live runner targeting `pg_addr` (e.g. `127.0.0.1:5432`).
    pub fn new(pg_addr: impl Into<String>) -> Self {
        Self {
            pg_addr: pg_addr.into(),
        }
    }
}

#[async_trait::async_trait]
impl ApiSqlRunner for LivePgSqlRunner {
    async fn execute_as_user(
        &self,
        username: &str,
        database: Option<&str>,
        sql: &str,
        max_rows: usize,
    ) -> Result<SqlQueryResult, ApiError> {
        let db = database.unwrap_or("postgres");
        PgConnection::query_simple(&self.pg_addr, Some(username), Some(db), sql, max_rows)
            .await
            .map_err(|e| ApiError::Internal(format!("Database query failed: {e}")))
    }
}
