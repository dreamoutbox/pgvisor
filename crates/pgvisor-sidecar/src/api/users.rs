use axum::extract::{Extension, Path, State};
use axum::Json;
use pgvisor_core::api::{PgRole, TablePrivilege};

use super::api_state::ApiState;
use super::authenticated_user::AuthenticatedUser;
use super::error::ApiError;
use super::sql::is_valid_identifier;

fn parse_pg_bool(raw: &str) -> bool {
    raw.eq_ignore_ascii_case("t") || raw.eq_ignore_ascii_case("true")
}

/// Validates role identifiers accepted by `/api/users/*`.
pub(crate) fn validate_role_name(name: &str) -> Result<(), ApiError> {
    if !is_valid_identifier(name) {
        return Err(ApiError::BadRequest(format!(
            "Invalid role identifier: '{name}'"
        )));
    }
    Ok(())
}

/// Fetches all non-system PostgreSQL roles via `state.sql_runner`.
pub(crate) async fn query_all_roles(
    state: &ApiState,
    actor: &str,
) -> Result<Vec<PgRole>, ApiError> {
    let sql = "SELECT r.rolname, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole, r.rolreplication, r.rolsuper, r.rolconnlimit, COALESCE((SELECT string_agg(b.rolname, ',') FROM pg_auth_members m JOIN pg_roles b ON (m.roleid = b.oid) WHERE m.member = r.oid), '') as member_of FROM pg_roles r WHERE r.rolname NOT LIKE 'pg_%' AND r.rolname != 'postgres' ORDER BY r.rolname;";
    let res = state
        .sql_runner
        .execute_as_user(actor, None, sql, 200)
        .await?;

    let roles = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if row.len() < 8 {
                return None;
            }
            let rolname = row[0].clone();
            let member_of = if row[7].is_empty() {
                Vec::new()
            } else {
                row[7]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            Some(PgRole {
                rolname,
                rolcanlogin: parse_pg_bool(&row[1]),
                rolcreatedb: parse_pg_bool(&row[2]),
                rolcreaterole: parse_pg_bool(&row[3]),
                rolreplication: parse_pg_bool(&row[4]),
                rolsuper: parse_pg_bool(&row[5]),
                rolconnlimit: row[6].parse::<i32>().unwrap_or(-1),
                member_of,
            })
        })
        .collect();
    Ok(roles)
}

/// Handler for `GET /api/users`.
pub async fn list_users(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<Json<Vec<PgRole>>, ApiError> {
    let roles = query_all_roles(&state, &user.username).await?;
    Ok(Json(roles))
}

/// Handler for `GET /api/users/:role`.
pub async fn get_user(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
) -> Result<Json<PgRole>, ApiError> {
    validate_role_name(&role)?;
    let roles = query_all_roles(&state, &user.username).await?;
    let found = roles
        .into_iter()
        .find(|r| r.rolname == role)
        .ok_or_else(|| ApiError::NotFound(format!("Role '{role}' not found")))?;
    Ok(Json(found))
}

/// Handler for `GET /api/users/:role/privileges`.
pub async fn get_user_privileges(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
) -> Result<Json<Vec<TablePrivilege>>, ApiError> {
    validate_role_name(&role)?;
    let sql = format!(
        "SELECT t.table_name, has_table_privilege('{role}', quote_ident(t.table_name), 'SELECT') as can_select, has_table_privilege('{role}', quote_ident(t.table_name), 'INSERT') as can_insert, has_table_privilege('{role}', quote_ident(t.table_name), 'UPDATE') as can_update, has_table_privilege('{role}', quote_ident(t.table_name), 'DELETE') as can_delete, has_table_privilege('{role}', quote_ident(t.table_name), 'TRUNCATE') as can_truncate, has_table_privilege('{role}', quote_ident(t.table_name), 'REFERENCES') as can_references, has_table_privilege('{role}', quote_ident(t.table_name), 'TRIGGER') as can_trigger FROM information_schema.tables t WHERE t.table_schema = 'public' ORDER BY t.table_name;"
    );

    let res = state
        .sql_runner
        .execute_as_user(&user.username, None, &sql, 200)
        .await?;

    let privs = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if row.len() < 8 {
                return None;
            }
            Some(TablePrivilege {
                table_name: row[0].clone(),
                schema: "public".into(),
                select: parse_pg_bool(&row[1]),
                insert: parse_pg_bool(&row[2]),
                update: parse_pg_bool(&row[3]),
                delete: parse_pg_bool(&row[4]),
                truncate: parse_pg_bool(&row[5]),
                references: parse_pg_bool(&row[6]),
                trigger: parse_pg_bool(&row[7]),
            })
        })
        .collect();

    Ok(Json(privs))
}
