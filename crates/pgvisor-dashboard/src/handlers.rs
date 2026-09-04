use std::sync::Arc;
use std::time::Instant;

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::models::{
    ClusterOverview, NodeHealthState, NodeRole, NodeSummary, SqlQueryError, SqlQueryRequest,
    SqlQueryResult,
};
use crate::security::{SecurityError, SqlSecurityGuard};
use crate::templates::{NodesTemplate, OverviewTemplate, SqlConsoleTemplate};

/// Abstraction for executing SQL queries on PostgreSQL backends.
#[async_trait::async_trait]
pub trait SqlExecutor: Send + Sync {
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<SqlQueryResult, String>;
}

/// In-memory or proxy-connected executor for the dashboard console.
pub struct StandaloneSqlExecutor;

#[async_trait::async_trait]
impl SqlExecutor for StandaloneSqlExecutor {
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<SqlQueryResult, String> {
        let start = Instant::now();
        // Safe standard catalog query simulation for standalone dashboard mode
        let upper = sql.to_uppercase();
        let (columns, rows) = if upper.contains("PG_STAT_DATABASE") {
            (
                vec![
                    "datname".into(),
                    "numbackends".into(),
                    "xact_commit".into(),
                    "xact_rollback".into(),
                ],
                vec![
                    vec!["postgres".into(), "4".into(), "14285".into(), "12".into()],
                    vec!["pgvisor_app".into(), "18".into(), "98420".into(), "3".into()],
                ],
            )
        } else if upper.contains("VERSION()") {
            (
                vec!["version".into()],
                vec![vec![
                    "PostgreSQL 16.3 on x86_64-pc-linux-gnu, compiled by gcc, 64-bit".into(),
                ]],
            )
        } else {
            (
                vec!["result".into()],
                vec![vec![format!("Executed successfully: {}", sql)]],
            )
        };

        let row_count = rows.len().min(max_rows);
        let elapsed = start.elapsed().as_millis() as u64;

        Ok(SqlQueryResult {
            columns,
            rows: rows.into_iter().take(row_count).collect(),
            execution_time_ms: elapsed,
            row_count,
            truncated: false,
        })
    }
}

/// Shared application state for dashboard HTTP handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub overview: Arc<RwLock<ClusterOverview>>,
    pub security_guard: Arc<SqlSecurityGuard>,
    pub sql_executor: Arc<dyn SqlExecutor>,
    pub admin_token: Option<String>,
}

impl DashboardState {
    pub fn new(cluster_id: &str, admin_token: Option<String>) -> Self {
        let initial_overview = ClusterOverview {
            cluster_id: cluster_id.to_string(),
            current_term: 1,
            leader_id: Some(1),
            leader_address: Some("127.0.0.1:5432".to_string()),
            quorum_size: 2,
            total_nodes: 3,
            healthy_nodes: 3,
            last_backup_at: None,
            total_backups: 0,
            nodes: vec![
                NodeSummary {
                    node_id: 1,
                    address: "127.0.0.1:5432".into(),
                    role: NodeRole::Leader,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 0,
                    uptime_secs: 3600,
                    is_local: true,
                },
                NodeSummary {
                    node_id: 2,
                    address: "127.0.0.1:5433".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 64,
                    uptime_secs: 3580,
                    is_local: false,
                },
                NodeSummary {
                    node_id: 3,
                    address: "127.0.0.1:5434".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 128,
                    uptime_secs: 3550,
                    is_local: false,
                },
            ],
        };

        Self {
            overview: Arc::new(RwLock::new(initial_overview)),
            security_guard: Arc::new(SqlSecurityGuard::default()),
            sql_executor: Arc::new(StandaloneSqlExecutor),
            admin_token,
        }
    }
}

/// GET / -> Renders cluster overview dashboard
pub async fn get_overview(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let overview = state.overview.read().await;
    let template = OverviewTemplate { overview: &overview };
    template
        .render()
        .map(Html)
        .map_err(|e| {
            error!(?e, "Failed to render overview template");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// GET /nodes -> Renders detailed node health inspection
pub async fn get_nodes(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let overview = state.overview.read().await;
    let template = NodesTemplate {
        nodes: &overview.nodes,
    };
    template
        .render()
        .map(Html)
        .map_err(|e| {
            error!(?e, "Failed to render nodes template");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// GET /sql -> Renders interactive SQL console
pub async fn get_sql_console() -> Result<Html<String>, StatusCode> {
    let template = SqlConsoleTemplate {};
    template
        .render()
        .map(Html)
        .map_err(|e| {
            error!(?e, "Failed to render sql template");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// GET /api/status -> Returns cluster health status JSON
pub async fn api_status(
    State(state): State<Arc<DashboardState>>,
) -> Json<ClusterOverview> {
    let overview = state.overview.read().await.clone();
    Json(overview)
}

/// POST /api/sql -> Validates read-only safety, enforces statement timeout, executes SQL
pub async fn api_execute_sql(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<SqlQueryRequest>,
) -> Result<Json<SqlQueryResult>, (StatusCode, Json<SqlQueryError>)> {
    let max_rows = payload
        .max_rows
        .unwrap_or(state.security_guard.max_result_rows)
        .min(state.security_guard.max_result_rows);

    // 1. Validate read-only security constraint
    let safe_sql = match state.security_guard.validate_read_only(&payload.query) {
        Ok(sql) => sql,
        Err(err) => {
            warn!(query = %payload.query, ?err, "SQL security validation rejected query");
            let (status, code) = match err {
                SecurityError::MutationForbidden(_) => (StatusCode::FORBIDDEN, "MUTATION_FORBIDDEN"),
                SecurityError::MultiStatementForbidden(_) => (
                    StatusCode::BAD_REQUEST,
                    "MULTI_STATEMENT_FORBIDDEN",
                ),
                SecurityError::EmptyQuery => (StatusCode::BAD_REQUEST, "EMPTY_QUERY"),
                _ => (StatusCode::BAD_REQUEST, "SECURITY_ERROR"),
            };
            return Err((
                status,
                Json(SqlQueryError {
                    code: code.into(),
                    message: err.to_string(),
                }),
            ));
        }
    };

    info!(sql = %safe_sql, max_rows, "Executing guarded SQL console query");

    // 2. Execute query under statement timeout protection
    let timeout = state.security_guard.max_execution_timeout;
    let exec_res = tokio::time::timeout(timeout, state.sql_executor.execute(&safe_sql, max_rows)).await;

    match exec_res {
        Ok(Ok(result)) => Ok(Json(result)),
        Ok(Err(db_err)) => {
            error!(?db_err, "Database error executing console SQL");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SqlQueryError {
                    code: "EXECUTION_FAILED".into(),
                    message: db_err,
                }),
            ))
        }
        Err(_) => {
            warn!(?timeout, "Console SQL query timed out");
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(SqlQueryError {
                    code: "STATEMENT_TIMEOUT".into(),
                    message: format!("Query execution exceeded statement timeout of {:?}", timeout),
                }),
            ))
        }
    }
}
