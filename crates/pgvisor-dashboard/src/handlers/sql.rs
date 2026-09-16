use std::sync::Arc;
use std::time::Instant;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::Deserialize;
use tracing::{error, info, warn};

use super::state::DashboardState;
use crate::models::{
    ColumnInfo, SqlQueryError, SqlQueryRequest, SqlQueryResult, TableDataResponse, TableSummary,
};
use crate::security::SecurityError;
use crate::templates::TablesTemplate;

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
        let (columns, rows) = if upper.contains("INFORMATION_SCHEMA.TABLES") {
            (
                vec!["table_name".into(), "table_schema".into()],
                vec![vec!["pgvisor_demo".into(), "public".into()]],
            )
        } else if upper.contains("INFORMATION_SCHEMA.COLUMNS") {
            (
                vec![
                    "column_name".into(),
                    "data_type".into(),
                    "is_nullable".into(),
                    "column_default".into(),
                ],
                vec![
                    vec![
                        "id".into(),
                        "integer".into(),
                        "NO".into(),
                        "nextval(...)".into(),
                    ],
                    vec![
                        "name".into(),
                        "character varying".into(),
                        "NO".into(),
                        "NULL".into(),
                    ],
                    vec![
                        "status".into(),
                        "character varying".into(),
                        "YES".into(),
                        "'active'".into(),
                    ],
                    vec!["counter".into(), "integer".into(), "YES".into(), "0".into()],
                    vec![
                        "created_at".into(),
                        "timestamp with time zone".into(),
                        "YES".into(),
                        "now()".into(),
                    ],
                    vec![
                        "updated_at".into(),
                        "timestamp with time zone".into(),
                        "YES".into(),
                        "now()".into(),
                    ],
                ],
            )
        } else if upper.contains("COUNT(*)") {
            (vec!["count".into()], vec![vec!["4".into()]])
        } else if upper.contains("PGVISOR_DEMO") {
            (
                vec![
                    "id".into(),
                    "name".into(),
                    "status".into(),
                    "counter".into(),
                    "created_at".into(),
                    "updated_at".into(),
                ],
                vec![
                    vec![
                        "1".into(),
                        "alpha".into(),
                        "active".into(),
                        "10".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "2".into(),
                        "beta".into(),
                        "active".into(),
                        "20".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "3".into(),
                        "gamma".into(),
                        "active".into(),
                        "130".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "4".into(),
                        "delta".into(),
                        "archived".into(),
                        "40".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                ],
            )
        } else if upper.contains("PG_STAT_DATABASE") {
            (
                vec![
                    "datname".into(),
                    "numbackends".into(),
                    "xact_commit".into(),
                    "xact_rollback".into(),
                ],
                vec![
                    vec!["postgres".into(), "4".into(), "14285".into(), "12".into()],
                    vec![
                        "pgvisor_app".into(),
                        "18".into(),
                        "98420".into(),
                        "3".into(),
                    ],
                ],
            )
        } else if upper.contains("VERSION()") {
            (
                vec!["version".into()],
                vec![vec![
                    "PostgreSQL 18.6 on x86_64-pc-linux-gnu, compiled by gcc, 64-bit".into(),
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

#[derive(Debug, Default, Deserialize)]
pub struct TablesQuery {
    pub table: Option<String>,
    pub tab: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub sql: Option<String>,
}

/// GET /tables -> Renders database table browser and merged SQL explorer
pub async fn get_tables_page(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<TablesQuery>,
) -> Result<Html<String>, StatusCode> {
    let tables_sql = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";
    let tables_res = state
        .sql_executor
        .execute(tables_sql, 100)
        .await
        .unwrap_or_else(|_| SqlQueryResult {
            columns: vec!["table_name".into()],
            rows: Vec::new(),
            execution_time_ms: 0,
            row_count: 0,
            truncated: false,
        });

    let tables: Vec<TableSummary> = tables_res
        .rows
        .into_iter()
        .filter_map(|mut row| {
            let name = row.drain(..).next()?;
            Some(TableSummary {
                name,
                schema: "public".into(),
                estimated_rows: 0,
                size_pretty: "-".into(),
            })
        })
        .collect();

    let active_table_name = params.table.as_deref().or_else(|| {
        if params.tab.as_deref() == Some("sql") {
            None
        } else {
            tables.first().map(|t| t.name.as_str())
        }
    });

    let active_tab = params.tab.as_deref().unwrap_or("data");

    let mut columns = Vec::new();
    let mut data_columns = Vec::new();
    let mut data_rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut total_rows = 0u64;
    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    if let Some(tbl) = active_table_name {
        if tbl.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            if active_tab == "schema" {
                let schema_sql = format!(
                    "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '{}' ORDER BY ordinal_position;",
                    tbl
                );
                if let Ok(res) = state.sql_executor.execute(&schema_sql, 200).await {
                    for row in res.rows {
                        if row.len() >= 3 {
                            let name = row[0].clone();
                            let data_type = row[1].clone();
                            let is_nullable = row[2].eq_ignore_ascii_case("YES");
                            let default_val =
                                if row.len() > 3 && !row[3].is_empty() && row[3] != "NULL" {
                                    Some(row[3].clone())
                                } else {
                                    None
                                };
                            let is_primary_key = name.eq_ignore_ascii_case("id");
                            columns.push(ColumnInfo {
                                name,
                                data_type,
                                is_nullable,
                                default_value: default_val,
                                is_primary_key,
                            });
                        }
                    }
                }
            } else if active_tab == "data" {
                let count_sql = format!("SELECT count(*) FROM {};", tbl);
                if let Ok(count_res) = state.sql_executor.execute(&count_sql, 1).await {
                    if let Some(r) = count_res.rows.first() {
                        if let Some(c_str) = r.first() {
                            total_rows = c_str.parse().unwrap_or(0);
                        }
                    }
                }

                let data_sql = format!("SELECT * FROM {} LIMIT {} OFFSET {};", tbl, limit, offset);
                if let Ok(data_res) = state.sql_executor.execute(&data_sql, limit).await {
                    data_columns = data_res.columns;
                    data_rows = data_res
                        .rows
                        .into_iter()
                        .map(|row| row.into_iter().map(Some).collect())
                        .collect();
                }
            }
        }
    }

    let total_pages = if total_rows > 0 {
        ((total_rows as usize + limit - 1) / limit).max(1)
    } else if !data_rows.is_empty() {
        1
    } else {
        0
    };

    let template = TablesTemplate {
        tables: &tables,
        active_table: active_table_name,
        active_tab,
        columns: &columns,
        data_columns: &data_columns,
        data_rows: &data_rows,
        total_rows,
        page,
        limit,
        total_pages,
        initial_sql: params.sql.as_deref(),
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render tables template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /sql -> Renders interactive SQL console (delegates to unified explorer in SQL tab)
pub async fn get_sql_console(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    get_tables_page(
        State(state),
        Query(TablesQuery {
            tab: Some("sql".into()),
            ..Default::default()
        }),
    )
    .await
}

/// GET /api/tables -> Returns JSON list of public tables
pub async fn api_list_tables(State(state): State<Arc<DashboardState>>) -> Json<Vec<TableSummary>> {
    let sql = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";
    let res = state
        .sql_executor
        .execute(sql, 200)
        .await
        .unwrap_or_else(|_| SqlQueryResult {
            columns: vec!["table_name".into()],
            rows: Vec::new(),
            execution_time_ms: 0,
            row_count: 0,
            truncated: false,
        });

    let tables = res
        .rows
        .into_iter()
        .filter_map(|mut row| {
            let name = row.drain(..).next()?;
            Some(TableSummary {
                name,
                schema: "public".into(),
                estimated_rows: 0,
                size_pretty: "-".into(),
            })
        })
        .collect();

    Json(tables)
}

/// GET /api/tables/:table/schema -> Returns column metadata for a table
pub async fn api_table_schema(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
) -> Result<Json<Vec<ColumnInfo>>, (StatusCode, String)> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let sql = format!(
        "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '{}' ORDER BY ordinal_position;",
        table
    );
    let res = state
        .sql_executor
        .execute(&sql, 200)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let columns = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if row.len() >= 3 {
                let name = row[0].clone();
                let data_type = row[1].clone();
                let is_nullable = row[2].eq_ignore_ascii_case("YES");
                let default_val = if row.len() > 3 && !row[3].is_empty() && row[3] != "NULL" {
                    Some(row[3].clone())
                } else {
                    None
                };
                let is_primary_key = name.eq_ignore_ascii_case("id");
                Some(ColumnInfo {
                    name,
                    data_type,
                    is_nullable,
                    default_value: default_val,
                    is_primary_key,
                })
            } else {
                None
            }
        })
        .collect();

    Ok(Json(columns))
}

/// GET /api/tables/:table/data?limit=50&offset=0 -> Returns paginated rows
pub async fn api_table_data(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
) -> Result<Json<TableDataResponse>, (StatusCode, String)> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let count_sql = format!("SELECT count(*) FROM {};", table);
    let total_rows = state
        .sql_executor
        .execute(&count_sql, 1)
        .await
        .ok()
        .and_then(|r| r.rows.first()?.first()?.parse::<u64>().ok())
        .unwrap_or(0);

    let sql = format!("SELECT * FROM {} LIMIT {} OFFSET {};", table, limit, offset);
    let res = state
        .sql_executor
        .execute(&sql, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(TableDataResponse {
        table_name: table,
        columns: res.columns,
        rows: res
            .rows
            .into_iter()
            .map(|r| r.into_iter().map(Some).collect())
            .collect(),
        total_rows,
        limit,
        offset,
    }))
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

    // 1. Validate SQL query according to security settings
    let safe_sql = match state.security_guard.validate_sql(&payload.query) {
        Ok(sql) => sql,
        Err(err) => {
            warn!(query = %payload.query, ?err, "SQL security validation rejected query");
            let (status, code) = match err {
                SecurityError::MutationForbidden(_) => {
                    (StatusCode::FORBIDDEN, "MUTATION_FORBIDDEN")
                }
                SecurityError::MultiStatementForbidden(_) => {
                    (StatusCode::BAD_REQUEST, "MULTI_STATEMENT_FORBIDDEN")
                }
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
    let exec_res =
        tokio::time::timeout(timeout, state.sql_executor.execute(&safe_sql, max_rows)).await;

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
                    message: format!(
                        "Query execution exceeded statement timeout of {:?}",
                        timeout
                    ),
                }),
            ))
        }
    }
}
