use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pgvisor_core::api::{
    ColumnInfo, SqlQueryError, SqlQueryRequest, TableDataResponse, TableSummary,
};
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
        Ok(Ok(result)) => Json(result).into_response(),
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
