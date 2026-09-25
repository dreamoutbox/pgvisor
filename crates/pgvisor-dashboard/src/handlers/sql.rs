use std::sync::Arc;
use std::time::Instant;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use pgvisor_core::audit::AuditEventKind;
use serde::Deserialize;
use tracing::{error, info, warn};

use super::state::DashboardState;
use crate::models::{
    ColumnInfo, DeleteRowRequest, RowMutationResponse, SqlQueryError, SqlQueryRequest,
    SqlQueryResult, TableDataResponse, TableSummary, UpdateRowRequest,
};
use crate::security::SecurityError;
use crate::templates::TablesTemplate;

/// Validate an SQL identifier to ensure it strictly conforms to PostgreSQL standard naming
/// (starts with alphabetic char or underscore, alphanumeric/underscore, <= 63 chars).
pub fn is_valid_identifier(ident: &str) -> bool {
    !ident.is_empty()
        && ident.len() <= 63
        && ident.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Format a JSON value into a safe SQL literal for queries.
pub fn format_sql_literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => {
            if *b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            let json_str = serde_json::to_string(value).unwrap_or_default();
            format!("'{}'", json_str.replace('\'', "''"))
        }
    }
}

/// Fetch primary key column names for a given table from the PostgreSQL catalog.
pub async fn fetch_table_primary_keys(sql_executor: &dyn SqlExecutor, table: &str) -> Vec<String> {
    if !is_valid_identifier(table) {
        return Vec::new();
    }
    let pk_sql = format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc JOIN information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema WHERE tc.table_name = '{}' AND tc.table_schema = 'public' AND tc.constraint_type = 'PRIMARY KEY' ORDER BY kcu.ordinal_position;",
        table
    );
    if let Ok(res) = sql_executor.execute(&pk_sql, 100).await {
        res.rows
            .into_iter()
            .filter_map(|mut r| r.drain(..).next())
            .collect()
    } else {
        Vec::new()
    }
}

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
        let (columns, rows) = if upper.starts_with("DELETE FROM") {
            (vec!["status".into()], vec![vec!["DELETE 1".into()]])
        } else if upper.starts_with("UPDATE") {
            (vec!["status".into()], vec![vec!["UPDATE 1".into()]])
        } else if upper.contains("TABLE_CONSTRAINTS") || upper.contains("KEY_COLUMN_USAGE") {
            (vec!["column_name".into()], vec![vec!["id".into()]])
        } else if upper.contains("INFORMATION_SCHEMA.TABLES") {
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
    let mut primary_keys = Vec::new();
    let mut total_rows = 0u64;
    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    if let Some(tbl) = active_table_name {
        if is_valid_identifier(tbl) {
            primary_keys = fetch_table_primary_keys(&*state.sql_executor, tbl).await;
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
                            let is_primary_key =
                                primary_keys.iter().any(|pk| pk.eq_ignore_ascii_case(&name));
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
        primary_keys: &primary_keys,
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
    if !is_valid_identifier(&table) {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let primary_keys = fetch_table_primary_keys(&*state.sql_executor, &table).await;
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
                let is_primary_key = primary_keys.iter().any(|pk| pk.eq_ignore_ascii_case(&name));
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
    if !is_valid_identifier(&table) {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let primary_keys = fetch_table_primary_keys(&*state.sql_executor, &table).await;

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
        primary_keys,
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

/// DELETE /api/tables/:table/rows -> Delete a single row by primary key
pub async fn api_delete_table_row(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
    Json(payload): Json<DeleteRowRequest>,
) -> Result<Json<RowMutationResponse>, (StatusCode, String)> {
    if !is_valid_identifier(&table) {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    if payload.primary_keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Primary keys must be provided to identify row".into(),
        ));
    }

    let pks = fetch_table_primary_keys(&*state.sql_executor, &table).await;
    if pks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Table '{}' has no primary key defined; row deletion is disabled",
                table
            ),
        ));
    }

    // Ensure all defined primary keys are provided in payload
    for pk in &pks {
        if !payload.primary_keys.contains_key(pk) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Missing required primary key column: '{}'", pk),
            ));
        }
    }

    // Validate identifiers and construct WHERE clauses
    let mut where_clauses = Vec::with_capacity(pks.len());
    for (col, val) in &payload.primary_keys {
        if !is_valid_identifier(col) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid primary key column identifier: '{}'", col),
            ));
        }
        if !pks.iter().any(|p| p.eq_ignore_ascii_case(col)) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Column '{}' is not part of the primary key for '{}'",
                    col, table
                ),
            ));
        }
        if val.is_null() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Primary key column '{}' value cannot be null", col),
            ));
        }
        where_clauses.push(format!("\"{}\" = {}", col, format_sql_literal(val)));
    }

    let sql = format!(
        "DELETE FROM \"{}\" WHERE {};",
        table,
        where_clauses.join(" AND ")
    );

    let res = state
        .sql_executor
        .execute(&sql, 1)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let affected_rows = if let Some(r) = res.rows.first() {
        if let Some(s) = r.first() {
            if s.starts_with("DELETE ") {
                s.trim_start_matches("DELETE ")
                    .trim()
                    .parse::<usize>()
                    .unwrap_or(1)
            } else {
                1
            }
        } else {
            1
        }
    } else {
        1
    };

    state
        .audit_log
        .append(
            AuditEventKind::DangerousSql,
            None,
            None,
            format!(
                "Deleted row from table '{}' (primary keys: {:?})",
                table, payload.primary_keys
            ),
            Some(
                serde_json::json!({
                    "table": table,
                    "primary_keys": payload.primary_keys,
                    "affected_rows": affected_rows,
                })
                .to_string(),
            ),
        )
        .await;

    Ok(Json(RowMutationResponse {
        status: "ok".into(),
        message: format!("Row deleted successfully from '{}'", table),
        affected_rows,
    }))
}

/// PUT /api/tables/:table/rows -> Update columns for a row identified by primary key
pub async fn api_update_table_row(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
    Json(payload): Json<UpdateRowRequest>,
) -> Result<Json<RowMutationResponse>, (StatusCode, String)> {
    if !is_valid_identifier(&table) {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    if payload.primary_keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Primary keys must be provided to identify row".into(),
        ));
    }

    if payload.values.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No column values provided to update".into(),
        ));
    }

    let pks = fetch_table_primary_keys(&*state.sql_executor, &table).await;
    if pks.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Table '{}' has no primary key defined; row updates are disabled",
                table
            ),
        ));
    }

    // Ensure all defined primary keys are provided
    for pk in &pks {
        if !payload.primary_keys.contains_key(pk) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Missing required primary key column: '{}'", pk),
            ));
        }
    }

    let mut where_clauses = Vec::with_capacity(pks.len());
    for (col, val) in &payload.primary_keys {
        if !is_valid_identifier(col) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid primary key column identifier: '{}'", col),
            ));
        }
        if !pks.iter().any(|p| p.eq_ignore_ascii_case(col)) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Column '{}' is not part of the primary key for '{}'",
                    col, table
                ),
            ));
        }
        if val.is_null() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Primary key column '{}' value cannot be null", col),
            ));
        }
        where_clauses.push(format!("\"{}\" = {}", col, format_sql_literal(val)));
    }

    let mut set_clauses = Vec::with_capacity(payload.values.len());
    for (col, val) in &payload.values {
        if !is_valid_identifier(col) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid column identifier: '{}'", col),
            ));
        }
        if pks.iter().any(|p| p.eq_ignore_ascii_case(col)) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Primary key column '{}' cannot be updated directly; delete and insert instead",
                    col
                ),
            ));
        }
        set_clauses.push(format!("\"{}\" = {}", col, format_sql_literal(val)));
    }

    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE {};",
        table,
        set_clauses.join(", "),
        where_clauses.join(" AND ")
    );

    let res = state
        .sql_executor
        .execute(&sql, 1)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let affected_rows = if let Some(r) = res.rows.first() {
        if let Some(s) = r.first() {
            if s.starts_with("UPDATE ") {
                s.trim_start_matches("UPDATE ")
                    .trim()
                    .parse::<usize>()
                    .unwrap_or(1)
            } else {
                1
            }
        } else {
            1
        }
    } else {
        1
    };

    state
        .audit_log
        .append(
            AuditEventKind::DangerousSql,
            None,
            None,
            format!(
                "Updated row in table '{}' (primary keys: {:?}, updated columns: {:?})",
                table,
                payload.primary_keys,
                payload.values.keys().collect::<Vec<_>>()
            ),
            Some(
                serde_json::json!({
                    "table": table,
                    "primary_keys": payload.primary_keys,
                    "values": payload.values,
                    "affected_rows": affected_rows,
                })
                .to_string(),
            ),
        )
        .await;

    Ok(Json(RowMutationResponse {
        status: "ok".into(),
        message: format!("Row updated successfully in '{}'", table),
        affected_rows,
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
