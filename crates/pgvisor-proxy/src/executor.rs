use pgvisor_core::api::SqlQueryResult;
use pgvisor_core::pg_client::{execute_simple_query_on_stream, PgClientError};
use pgvisor_core::sql_guard::SqlSecurityGuard;
use pgvisor_dashboard::handlers::SqlExecutor;

use crate::pool::{BackendRole, ConnectionPool, FailoverConfig};

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
    if upper.contains("PG_STAT_REPLICATION") {
        return true;
    }
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
            failover_config: FailoverConfig {
                failover_timeout: std::time::Duration::from_secs(3),
                retry_interval: std::time::Duration::from_millis(200),
            },
        }
    }

    /// Executes a SQL query directly against a specific backend role (Leader or Standby).
    ///
    /// # Arguments
    /// * `sql` - SQL string to execute
    /// * `max_rows` - Maximum number of rows to return
    /// * `role` - `BackendRole::Leader` or `BackendRole::Standby`
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn doc_example(executor: &pgvisor_proxy::executor::ProxySqlExecutor) {
    /// let res = executor.execute_with_role("SELECT 1", 1, pgvisor_proxy::pool::BackendRole::Leader).await;
    /// # }
    /// ```
    pub async fn execute_with_role(
        &self,
        sql: &str,
        max_rows: usize,
        role: BackendRole,
    ) -> Result<SqlQueryResult, String> {
        self.execute_with_role_and_db(sql, max_rows, role, None).await
    }

    /// Executes a SQL query directly against a specific backend role (Leader or Standby)
    /// targeting an optional specific database.
    pub async fn execute_with_role_and_db(
        &self,
        sql: &str,
        max_rows: usize,
        role: BackendRole,
        database: Option<&str>,
    ) -> Result<SqlQueryResult, String> {
        // Acquire a connection from the pool for the required role and database
        let mut backend = self
            .pool
            .acquire_with_retry(role, &self.failover_config, None, database)
            .await
            .map_err(|e| format!("Failed to acquire backend connection: {}", e))?;

        let result = execute_simple_query_on_stream(&mut backend.stream, sql, max_rows).await;

        match result {
            Ok(query_result) => {
                self.pool.release(backend).await;
                Ok(query_result)
            }
            Err(PgClientError::SqlError(err)) => {
                // Backend returned ReadyForQuery after ErrorResponse; connection is still healthy.
                self.pool.release(backend).await;
                Err(err)
            }
            Err(err) => Err(err.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl SqlExecutor for ProxySqlExecutor {
    async fn execute_on_db(
        &self,
        sql: &str,
        max_rows: usize,
        db: Option<&str>,
    ) -> Result<SqlQueryResult, String> {
        let role = if is_leader_required(sql) {
            BackendRole::Leader
        } else {
            BackendRole::Standby
        };
        self.execute_with_role_and_db(sql, max_rows, role, db).await
    }
}
