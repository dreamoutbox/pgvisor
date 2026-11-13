use axum::extract::{Extension, Path, State};
use axum::Json;
use pgvisor_core::api::{
    AlterRoleRequest, CreateRoleRequest, PgRole, RoleMembershipRequest, TablePrivilege,
    TablePrivilegeRequest,
};
use pgvisor_core::audit::AuditEventKind;

use super::api_state::ApiState;
use super::authenticated_user::AuthenticatedUser;
use super::error::ApiError;
use super::members::require_leader;
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

async fn run_user_ddl(
    state: &ApiState,
    user: &AuthenticatedUser,
    guard_ddl: &str,
    actual_ddl: &str,
    audit_detail: String,
) -> Result<(), ApiError> {
    require_leader(state).await?;
    state
        .security_guard
        .validate_user_management_ddl(guard_ddl)
        .map_err(|e| ApiError::Forbidden(e.to_string()))?;

    state
        .sql_runner
        .execute_as_user(&user.username, None, actual_ddl, 1)
        .await?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            Some(state.sidecar.node_id),
            Some(&state.pg_addr),
            format!("{audit_detail} (actor: '{}')", user.username),
            None,
        )
        .await;

    Ok(())
}

/// Handler for `POST /api/users` (leader-only).
pub async fn create_user(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(req): Json<CreateRoleRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&req.name)?;
    let mut ddl = format!(
        "CREATE ROLE \"{}\" WITH {} {} {} {} CONNECTION LIMIT {}",
        req.name,
        if req.login { "LOGIN" } else { "NOLOGIN" },
        if req.createdb { "CREATEDB" } else { "NOCREATEDB" },
        if req.createrole {
            "CREATEROLE"
        } else {
            "NOCREATEROLE"
        },
        if req.replication {
            "REPLICATION"
        } else {
            "NOREPLICATION"
        },
        req.connection_limit
    );
    if let Some(pwd) = req.password.as_deref().filter(|p| !p.is_empty()) {
        let escaped_pwd = pwd.replace('\'', "''");
        ddl.push_str(&format!(" PASSWORD '{escaped_pwd}'"));
    }
    ddl.push(';');

    let guard_ddl = format!("CREATE ROLE \"{}\" WITH LOGIN;", req.name);
    let detail = format!(
        "Database role '{}' created (login={}, createdb={}, createrole={})",
        req.name, req.login, req.createdb, req.createrole
    );
    run_user_ddl(&state, &user, &guard_ddl, &ddl, detail).await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' created successfully", req.name),
    })))
}

/// Handler for `DELETE /api/users/:role` (leader-only).
pub async fn drop_user(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&role)?;
    let ddl = format!("DROP ROLE \"{role}\";");
    run_user_ddl(
        &state,
        &user,
        &ddl,
        &ddl,
        format!("Database role '{role}' dropped"),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{role}' dropped successfully"),
    })))
}

/// Handler for `PUT /api/users/:role` (leader-only).
pub async fn alter_user(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
    Json(req): Json<AlterRoleRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&role)?;
    let mut clauses = Vec::new();
    if let Some(login) = req.login {
        clauses.push(if login { "LOGIN" } else { "NOLOGIN" }.to_string());
    }
    if let Some(createdb) = req.createdb {
        clauses.push(
            if createdb { "CREATEDB" } else { "NOCREATEDB" }.to_string(),
        );
    }
    if let Some(createrole) = req.createrole {
        clauses.push(
            if createrole {
                "CREATEROLE"
            } else {
                "NOCREATEROLE"
            }
            .to_string(),
        );
    }
    if let Some(replication) = req.replication {
        clauses.push(
            if replication {
                "REPLICATION"
            } else {
                "NOREPLICATION"
            }
            .to_string(),
        );
    }
    if let Some(conn_limit) = req.connection_limit {
        clauses.push(format!("CONNECTION LIMIT {conn_limit}"));
    }
    if let Some(pwd) = req.password.as_deref().filter(|p| !p.is_empty()) {
        let escaped_pwd = pwd.replace('\'', "''");
        clauses.push(format!("PASSWORD '{escaped_pwd}'"));
    }

    let guard_ddl = format!("ALTER ROLE \"{role}\" WITH LOGIN;");
    if clauses.is_empty() {
        require_leader(&state).await?;
        state
            .security_guard
            .validate_user_management_ddl(&guard_ddl)
            .map_err(|e| ApiError::Forbidden(e.to_string()))?;
    } else {
        let ddl = format!("ALTER ROLE \"{role}\" WITH {};", clauses.join(" "));
        run_user_ddl(
            &state,
            &user,
            &guard_ddl,
            &ddl,
            format!("Database role '{role}' attributes altered"),
        )
        .await?;
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{role}' updated successfully"),
    })))
}

/// Handler for `POST /api/users/:role/memberships` (leader-only).
pub async fn grant_membership(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
    Json(req): Json<RoleMembershipRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&role)?;
    validate_role_name(&req.group_role)?;
    let ddl = format!("GRANT \"{}\" TO \"{role}\";", req.group_role);
    run_user_ddl(
        &state,
        &user,
        &ddl,
        &ddl,
        format!("Granted group '{}' to role '{role}'", req.group_role),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Granted group '{}' to role '{role}'", req.group_role),
    })))
}

/// Handler for `DELETE /api/users/:role/memberships/:group` (leader-only).
pub async fn revoke_membership(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path((role, group)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&role)?;
    validate_role_name(&group)?;
    let ddl = format!("REVOKE \"{group}\" FROM \"{role}\";");
    run_user_ddl(
        &state,
        &user,
        &ddl,
        &ddl,
        format!("Revoked group '{group}' from role '{role}'"),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Revoked group '{group}' from role '{role}'"),
    })))
}

/// Handler for `POST /api/users/:role/privileges` (leader-only).
pub async fn set_user_privilege(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(role): Path<String>,
    Json(req): Json<TablePrivilegeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_role_name(&role)?;
    validate_role_name(&req.table_name)?;
    let schema = req.schema.as_deref().unwrap_or("public");
    validate_role_name(schema)?;

    let (verb, prep, action_word, to_from) = if req.grant {
        ("GRANT", "TO", "Granted", "to")
    } else {
        ("REVOKE", "FROM", "Revoked", "from")
    };
    let priv_sql = req.privilege.as_sql_str();
    let guard_ddl = format!(
        "{verb} {priv_sql} ON TABLE {schema}.\"{}\" {prep} \"{role}\";",
        req.table_name
    );
    let actual_ddl = format!(
        "{verb} {priv_sql} ON TABLE \"{schema}\".\"{}\" {prep} \"{role}\";",
        req.table_name
    );
    run_user_ddl(
        &state,
        &user,
        &guard_ddl,
        &actual_ddl,
        format!(
            "{action_word} table privilege {priv_sql} on '{}' {to_from} role '{role}'",
            req.table_name
        ),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("{action_word} {priv_sql} on {} {to_from} role {role}", req.table_name),
    })))
}
