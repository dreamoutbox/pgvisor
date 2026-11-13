use std::collections::HashMap;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pgvisor_core::api::{
    ColumnInfo, DeleteRowRequest, RowMutationResponse, SqlQueryError, SqlQueryRequest,
    SqlQueryResult, TableDataResponse, TableSummary, UpdateRowRequest,
};
use pgvisor_core::audit::AuditEventKind;
use pgvisor_core::sql_guard::SecurityError;
use serde::Deserialize;
use tracing::{error, info, warn};

use super::api_state::ApiState;
use super::authenticated_user::AuthenticatedUser;
use super::error::ApiError;
use super::members::require_leader;

const DEFAULT_TABLE_PAGE_SIZE: usize = 50;
const MAX_TABLE_PAGE_SIZE: usize = 100;

/// Query parameters for table inspection endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct TablesQuery {
    pub db: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
}

/// Validates a PostgreSQL identifier (starts with ASCII letter/`_`, alphanumeric/`_`, <= 63 chars).
///
/// # Arguments
/// * `ident` - Candidate table, column, role, or database name.
pub fn is_valid_identifier(ident: &str) -> bool {
    !ident.is_empty()
        && ident.len() <= 63
        && ident.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_optional_db(db: Option<&str>) -> Result<(), ApiError> {
    if let Some(name) = db {
        if !is_valid_identifier(name) {
            return Err(ApiError::BadRequest(format!(
                "Invalid database identifier: '{name}'"
            )));
        }
    }
    Ok(())
}

/// Fetches public-schema primary key column names for `table` as `username`.
pub(crate) async fn fetch_table_primary_keys(
    state: &ApiState,
    username: &str,
    table: &str,
    db: Option<&str>,
) -> Vec<String> {
    if !is_valid_identifier(table) {
        return Vec::new();
    }
    let pk_sql = format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc JOIN information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema WHERE tc.table_name = '{table}' AND tc.table_schema = 'public' AND tc.constraint_type = 'PRIMARY KEY' ORDER BY kcu.ordinal_position;"
    );
    match state
        .sql_runner
        .execute_as_user(username, db, &pk_sql, 100)
        .await
    {
        Ok(res) => res
            .rows
            .into_iter()
            .filter_map(|mut r| r.drain(..).next())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Handler for `GET /api/databases`.
pub async fn list_databases(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<Json<Vec<String>>, ApiError> {
    let sql = "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname ASC;";
    let res = state
        .sql_runner
        .execute_as_user(&user.username, None, sql, 100)
        .await?;
    let dbs = res
        .rows
        .into_iter()
        .filter_map(|mut r| r.drain(..).next())
        .collect();
    Ok(Json(dbs))
}

/// Handler for `GET /api/tables`.
pub async fn list_tables(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(params): Query<TablesQuery>,
) -> Result<Json<Vec<TableSummary>>, ApiError> {
    validate_optional_db(params.db.as_deref())?;
    let sql = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";
    let res = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), sql, 200)
        .await?;

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
    Ok(Json(tables))
}

/// Handler for `GET /api/tables/:table/schema`.
pub async fn get_table_schema(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
) -> Result<Json<Vec<ColumnInfo>>, ApiError> {
    if !is_valid_identifier(&table) {
        return Err(ApiError::BadRequest(format!(
            "Invalid table name: '{table}'"
        )));
    }
    validate_optional_db(params.db.as_deref())?;

    let primary_keys =
        fetch_table_primary_keys(&state, &user.username, &table, params.db.as_deref()).await;
    let sql = format!(
        "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '{table}' ORDER BY ordinal_position;"
    );
    let res = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), &sql, 200)
        .await?;

    let columns = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if row.len() < 3 {
                return None;
            }
            let name = row[0].clone();
            let data_type = row[1].clone();
            let is_nullable = row[2].eq_ignore_ascii_case("YES");
            let default_value = if row.len() > 3 && !row[3].is_empty() && row[3] != "NULL" {
                Some(row[3].clone())
            } else {
                None
            };
            let is_primary_key = primary_keys.iter().any(|pk| pk.eq_ignore_ascii_case(&name));
            Some(ColumnInfo {
                name,
                data_type,
                is_nullable,
                default_value,
                is_primary_key,
            })
        })
        .collect();

    Ok(Json(columns))
}

/// Handler for `GET /api/tables/:table/data`.
pub async fn get_table_data(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
) -> Result<Json<TableDataResponse>, ApiError> {
    if !is_valid_identifier(&table) {
        return Err(ApiError::BadRequest(format!(
            "Invalid table name: '{table}'"
        )));
    }
    validate_optional_db(params.db.as_deref())?;

    let limit = params
        .limit
        .unwrap_or(DEFAULT_TABLE_PAGE_SIZE)
        .clamp(1, MAX_TABLE_PAGE_SIZE);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let primary_keys =
        fetch_table_primary_keys(&state, &user.username, &table, params.db.as_deref()).await;

    let count_sql = format!("SELECT count(*) FROM \"{table}\";");
    let total_rows = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), &count_sql, 1)
        .await
        .ok()
        .and_then(|r| r.rows.first()?.first()?.parse::<u64>().ok())
        .unwrap_or(0);

    let data_sql = format!("SELECT * FROM \"{table}\" LIMIT {limit} OFFSET {offset};");
    let res = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), &data_sql, limit)
        .await?;

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

fn sql_error_response(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(SqlQueryError {
            code: code.to_string(),
            message: message.into(),
        }),
    )
        .into_response()
}

/// Handler for `POST /api/sql`: validates with `SqlSecurityGuard`, allows reads anywhere,
/// and enforces leader routing for mutating statements.
pub async fn execute_sql(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(params): Query<TablesQuery>,
    Json(payload): Json<SqlQueryRequest>,
) -> Response {
    let target_db = payload.database.as_deref().or(params.db.as_deref());
    if let Some(db) = target_db {
        if !is_valid_identifier(db) {
            return sql_error_response(
                StatusCode::BAD_REQUEST,
                "INVALID_DATABASE",
                format!("Invalid database identifier: '{db}'"),
            );
        }
    }

    let max_rows = payload
        .max_rows
        .unwrap_or(state.security_guard.max_result_rows)
        .min(state.security_guard.max_result_rows);

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
            return sql_error_response(status, code, err.to_string());
        }
    };

    // Mutating queries require the cluster leader; read-only queries execute on any node.
    if state.security_guard.validate_read_only(&safe_sql).is_err() {
        if let Err(err) = require_leader(&state).await {
            return err.into_response();
        }
    }

    info!(
        actor = %user.username,
        sql = %safe_sql,
        max_rows,
        ?target_db,
        "Executing guarded SQL console query"
    );

    let timeout = state.security_guard.max_execution_timeout;
    let exec_res = tokio::time::timeout(
        timeout,
        state
            .sql_runner
            .execute_as_user(&user.username, target_db, &safe_sql, max_rows),
    )
    .await;

    match exec_res {
        Ok(Ok(result)) => {
            audit_executed_sql(&state, &user.username, target_db, &safe_sql).await;
            Json(result).into_response()
        }
        Ok(Err(err)) => {
            error!(?err, "Database error executing console SQL");
            sql_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "EXECUTION_FAILED",
                err.to_string(),
            )
        }
        Err(_) => {
            warn!(?timeout, "Console SQL query timed out");
            sql_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "STATEMENT_TIMEOUT",
                format!("Query execution exceeded statement timeout of {timeout:?}"),
            )
        }
    }
}

async fn audit_executed_sql(
    state: &ApiState,
    actor: &str,
    target_db: Option<&str>,
    sql: &str,
) {
    let upper = sql.trim().to_ascii_uppercase();
    let is_drop = upper.starts_with("DROP TABLE")
        || upper.starts_with("DROP DATABASE")
        || upper.starts_with("DROP SCHEMA");
    let is_truncate = upper.starts_with("TRUNCATE");
    let is_delete = if upper.starts_with("DELETE") {
        if std::env::var("PGVISOR_AUDIT_DELETE").is_ok() {
            true
        } else {
            !upper.contains("WHERE")
        }
    } else {
        false
    };

    let db = target_db.unwrap_or("default");
    let snippet = if sql.len() > 300 {
        format!("{}...", &sql[..300])
    } else {
        sql.to_string()
    };

    if is_drop || is_truncate || is_delete {
        let pitr_candidate = (chrono::Utc::now() - chrono::Duration::seconds(1))
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        state
            .audit_log
            .append(
                AuditEventKind::DangerousSql,
                Some(state.sidecar.node_id),
                Some(&state.pg_addr),
                format!("Dangerous SQL executed by '{actor}' on '{db}': {snippet}"),
                Some(pitr_candidate),
            )
            .await;
    }

    let is_user_permission = upper.starts_with("CREATE ROLE")
        || upper.starts_with("CREATE USER")
        || upper.starts_with("DROP ROLE")
        || upper.starts_with("DROP USER")
        || upper.starts_with("ALTER ROLE")
        || upper.starts_with("ALTER USER")
        || upper.starts_with("GRANT ")
        || upper.starts_with("REVOKE ");

    if is_user_permission {
        state
            .audit_log
            .append(
                AuditEventKind::UserPermission,
                Some(state.sidecar.node_id),
                Some(&state.pg_addr),
                format!("User/permission SQL executed by '{actor}' on '{db}': {snippet}"),
                None,
            )
            .await;
    }
}

fn format_sql_literal(value: &serde_json::Value) -> String {
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

fn build_pk_where_clauses(
    table: &str,
    table_pks: &[String],
    provided_pks: &HashMap<String, serde_json::Value>,
) -> Result<Vec<String>, ApiError> {
    if provided_pks.is_empty() {
        return Err(ApiError::BadRequest(
            "Primary keys must be provided to identify row".into(),
        ));
    }
    if table_pks.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "Table '{table}' has no primary key defined; row mutation is disabled"
        )));
    }
    for pk in table_pks {
        if !provided_pks.contains_key(pk) {
            return Err(ApiError::BadRequest(format!(
                "Missing required primary key column: '{pk}'"
            )));
        }
    }

    let mut clauses = Vec::with_capacity(table_pks.len());
    for (col, val) in provided_pks {
        if !is_valid_identifier(col) {
            return Err(ApiError::BadRequest(format!(
                "Invalid primary key column identifier: '{col}'"
            )));
        }
        if !table_pks.iter().any(|p| p.eq_ignore_ascii_case(col)) {
            return Err(ApiError::BadRequest(format!(
                "Column '{col}' is not part of the primary key for '{table}'"
            )));
        }
        if val.is_null() {
            return Err(ApiError::BadRequest(format!(
                "Primary key column '{col}' value cannot be null"
            )));
        }
        clauses.push(format!("\"{col}\" = {}", format_sql_literal(val)));
    }
    Ok(clauses)
}

fn parse_affected_rows(res: &SqlQueryResult, command_prefix: &str) -> usize {
    res.rows
        .first()
        .and_then(|r| r.first())
        .and_then(|s| s.strip_prefix(command_prefix))
        .and_then(|rest| rest.trim().parse::<usize>().ok())
        .unwrap_or(1)
}

/// Handler for `DELETE /api/tables/:table/rows` (leader-only).
pub async fn delete_table_row(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
    Json(payload): Json<DeleteRowRequest>,
) -> Result<Json<RowMutationResponse>, ApiError> {
    require_leader(&state).await?;
    if !is_valid_identifier(&table) {
        return Err(ApiError::BadRequest(format!(
            "Invalid table name: '{table}'"
        )));
    }
    validate_optional_db(params.db.as_deref())?;

    let pks = fetch_table_primary_keys(&state, &user.username, &table, params.db.as_deref()).await;
    let where_clauses = build_pk_where_clauses(&table, &pks, &payload.primary_keys)?;

    let sql = format!(
        "DELETE FROM \"{table}\" WHERE {};",
        where_clauses.join(" AND ")
    );
    let res = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), &sql, 1)
        .await?;
    let affected_rows = parse_affected_rows(&res, "DELETE ");

    state
        .audit_log
        .append(
            AuditEventKind::DangerousSql,
            Some(state.sidecar.node_id),
            Some(&state.pg_addr),
            format!(
                "Deleted row from table '{table}' by actor '{}' (primary keys: {:?})",
                user.username, payload.primary_keys
            ),
            Some(
                serde_json::json!({
                    "actor": user.username,
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
        message: format!("Row deleted successfully from '{table}'"),
        affected_rows,
    }))
}

/// Handler for `PUT /api/tables/:table/rows` (leader-only).
pub async fn update_table_row(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
    Json(payload): Json<UpdateRowRequest>,
) -> Result<Json<RowMutationResponse>, ApiError> {
    require_leader(&state).await?;
    if !is_valid_identifier(&table) {
        return Err(ApiError::BadRequest(format!(
            "Invalid table name: '{table}'"
        )));
    }
    validate_optional_db(params.db.as_deref())?;
    if payload.values.is_empty() {
        return Err(ApiError::BadRequest(
            "No column values provided to update".into(),
        ));
    }

    let pks = fetch_table_primary_keys(&state, &user.username, &table, params.db.as_deref()).await;
    let where_clauses = build_pk_where_clauses(&table, &pks, &payload.primary_keys)?;

    let mut set_clauses = Vec::with_capacity(payload.values.len());
    for (col, val) in &payload.values {
        if !is_valid_identifier(col) {
            return Err(ApiError::BadRequest(format!(
                "Invalid column identifier: '{col}'"
            )));
        }
        if pks.iter().any(|p| p.eq_ignore_ascii_case(col)) {
            return Err(ApiError::BadRequest(format!(
                "Primary key column '{col}' cannot be updated directly; delete and insert instead"
            )));
        }
        set_clauses.push(format!("\"{col}\" = {}", format_sql_literal(val)));
    }

    let sql = format!(
        "UPDATE \"{table}\" SET {} WHERE {};",
        set_clauses.join(", "),
        where_clauses.join(" AND ")
    );
    let res = state
        .sql_runner
        .execute_as_user(&user.username, params.db.as_deref(), &sql, 1)
        .await?;
    let affected_rows = parse_affected_rows(&res, "UPDATE ");

    state
        .audit_log
        .append(
            AuditEventKind::DangerousSql,
            Some(state.sidecar.node_id),
            Some(&state.pg_addr),
            format!(
                "Updated row in table '{table}' by actor '{}' (primary keys: {:?})",
                user.username, payload.primary_keys
            ),
            Some(
                serde_json::json!({
                    "actor": user.username,
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
        message: format!("Row updated successfully in '{table}'"),
        affected_rows,
    }))
}
