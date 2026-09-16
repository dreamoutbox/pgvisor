use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use pgvisor_core::audit::AuditEventKind;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::error;

use super::sql::{api_list_tables, SqlExecutor};
use super::state::DashboardState;
use crate::models::{
    AlterRoleRequest, CreateRoleRequest, PgRole, RoleMembershipRequest, TablePrivilege,
    TablePrivilegeKind, TablePrivilegeRequest,
};
use crate::templates::UsersTemplate;

/// Abstraction for managing database roles and permissions.
#[async_trait::async_trait]
pub trait UserService: Send + Sync {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String>;
    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String>;
    async fn get_table_privileges(&self, role: &str) -> Result<Vec<TablePrivilege>, String>;
    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String>;
    async fn drop_role(&self, name: &str) -> Result<(), String>;
    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String>;
    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String>;
    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String>;
    async fn set_table_privilege(
        &self,
        role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String>;
}

/// In-memory mock user service for standalone dashboard testing and demo mode.
pub struct StandaloneUserService {
    roles: Arc<RwLock<Vec<PgRole>>>,
    table_privileges: Arc<RwLock<Vec<TablePrivilege>>>,
}

impl StandaloneUserService {
    pub fn new() -> Self {
        let initial_roles = vec![
            PgRole {
                rolname: "app_user".into(),
                rolcanlogin: true,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: 50,
                member_of: vec!["read_only_group".into()],
            },
            PgRole {
                rolname: "analytics_worker".into(),
                rolcanlogin: true,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: 10,
                member_of: vec![],
            },
            PgRole {
                rolname: "read_only_group".into(),
                rolcanlogin: false,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: -1,
                member_of: vec![],
            },
        ];

        let initial_privs = vec![TablePrivilege {
            table_name: "pgvisor_demo".into(),
            schema: "public".into(),
            select: true,
            insert: false,
            update: false,
            delete: false,
            truncate: false,
            references: false,
            trigger: false,
        }];

        Self {
            roles: Arc::new(RwLock::new(initial_roles)),
            table_privileges: Arc::new(RwLock::new(initial_privs)),
        }
    }
}

impl Default for StandaloneUserService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl UserService for StandaloneUserService {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String> {
        let mut list = self.roles.read().await.clone();
        list.sort_by(|a, b| a.rolname.cmp(&b.rolname));
        Ok(list)
    }

    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String> {
        let list = self.roles.read().await;
        Ok(list.iter().find(|r| r.rolname == name).cloned())
    }

    async fn get_table_privileges(&self, _role: &str) -> Result<Vec<TablePrivilege>, String> {
        Ok(self.table_privileges.read().await.clone())
    }

    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String> {
        let mut list = self.roles.write().await;
        if list.iter().any(|r| r.rolname == req.name) {
            return Err(format!("Role '{}' already exists", req.name));
        }
        list.push(PgRole {
            rolname: req.name.clone(),
            rolcanlogin: req.login,
            rolcreatedb: req.createdb,
            rolcreaterole: req.createrole,
            rolreplication: req.replication,
            rolsuper: false,
            rolconnlimit: req.connection_limit,
            member_of: vec![],
        });
        Ok(())
    }

    async fn drop_role(&self, name: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let prev = list.len();
        list.retain(|r| r.rolname != name);
        if list.len() == prev {
            Err(format!("Role '{}' not found", name))
        } else {
            Ok(())
        }
    }

    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let role = list
            .iter_mut()
            .find(|r| r.rolname == name)
            .ok_or_else(|| format!("Role '{}' not found", name))?;

        if let Some(l) = req.login {
            role.rolcanlogin = l;
        }
        if let Some(db) = req.createdb {
            role.rolcreatedb = db;
        }
        if let Some(cr) = req.createrole {
            role.rolcreaterole = cr;
        }
        if let Some(rep) = req.replication {
            role.rolreplication = rep;
        }
        if let Some(limit) = req.connection_limit {
            role.rolconnlimit = limit;
        }
        Ok(())
    }

    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let target = list
            .iter_mut()
            .find(|r| r.rolname == role)
            .ok_or_else(|| format!("Role '{}' not found", role))?;

        if !target.member_of.iter().any(|m| m == group_role) {
            target.member_of.push(group_role.to_string());
        }
        Ok(())
    }

    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let target = list
            .iter_mut()
            .find(|r| r.rolname == role)
            .ok_or_else(|| format!("Role '{}' not found", role))?;

        target.member_of.retain(|m| m != group_role);
        Ok(())
    }

    async fn set_table_privilege(
        &self,
        _role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String> {
        let mut privs = self.table_privileges.write().await;
        let priv_idx = if let Some(idx) = privs.iter().position(|p| p.table_name == req.table_name)
        {
            idx
        } else {
            privs.push(TablePrivilege {
                table_name: req.table_name.clone(),
                schema: req.schema.clone().unwrap_or_else(|| "public".into()),
                select: false,
                insert: false,
                update: false,
                delete: false,
                truncate: false,
                references: false,
                trigger: false,
            });
            privs.len().saturating_sub(1)
        };

        if let Some(entry) = privs.get_mut(priv_idx) {
            match req.privilege {
                TablePrivilegeKind::Select => entry.select = req.grant,
                TablePrivilegeKind::Insert => entry.insert = req.grant,
                TablePrivilegeKind::Update => entry.update = req.grant,
                TablePrivilegeKind::Delete => entry.delete = req.grant,
                TablePrivilegeKind::Truncate => entry.truncate = req.grant,
                TablePrivilegeKind::References => entry.references = req.grant,
                TablePrivilegeKind::Trigger => entry.trigger = req.grant,
            }
        }
        Ok(())
    }
}

/// Live PostgreSQL user service executing safe DDL queries against the leader node.
pub struct SqlUserService {
    sql_executor: Arc<dyn SqlExecutor>,
}

impl SqlUserService {
    pub fn new(sql_executor: Arc<dyn SqlExecutor>) -> Self {
        Self { sql_executor }
    }

    fn validate_identifier(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("Identifier cannot be empty".into());
        }
        if name.len() > 63 {
            return Err("Identifier exceeds maximum length of 63 characters".into());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("Identifier '{}' contains invalid characters", name));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl UserService for SqlUserService {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String> {
        let sql = "SELECT r.rolname, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole, r.rolreplication, r.rolsuper, r.rolconnlimit, COALESCE((SELECT string_agg(b.rolname, ',') FROM pg_auth_members m JOIN pg_roles b ON (m.roleid = b.oid) WHERE m.member = r.oid), '') as member_of FROM pg_roles r WHERE r.rolname NOT LIKE 'pg_%' AND r.rolname != 'postgres' ORDER BY r.rolname;";
        let res = self.sql_executor.execute(sql, 200).await?;

        let roles = res
            .rows
            .into_iter()
            .filter_map(|row| {
                if row.len() >= 8 {
                    let rolname = row[0].clone();
                    let rolcanlogin =
                        row[1].eq_ignore_ascii_case("t") || row[1].eq_ignore_ascii_case("true");
                    let rolcreatedb =
                        row[2].eq_ignore_ascii_case("t") || row[2].eq_ignore_ascii_case("true");
                    let rolcreaterole =
                        row[3].eq_ignore_ascii_case("t") || row[3].eq_ignore_ascii_case("true");
                    let rolreplication =
                        row[4].eq_ignore_ascii_case("t") || row[4].eq_ignore_ascii_case("true");
                    let rolsuper =
                        row[5].eq_ignore_ascii_case("t") || row[5].eq_ignore_ascii_case("true");
                    let rolconnlimit = row[6].parse::<i32>().unwrap_or(-1);
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
                        rolcanlogin,
                        rolcreatedb,
                        rolcreaterole,
                        rolreplication,
                        rolsuper,
                        rolconnlimit,
                        member_of,
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(roles)
    }

    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String> {
        Self::validate_identifier(name)?;
        let roles = self.list_roles().await?;
        Ok(roles.into_iter().find(|r| r.rolname == name))
    }

    async fn get_table_privileges(&self, role: &str) -> Result<Vec<TablePrivilege>, String> {
        Self::validate_identifier(role)?;
        let sql = format!(
            "SELECT t.table_name, has_table_privilege('{}', quote_ident(t.table_name), 'SELECT') as can_select, has_table_privilege('{}', quote_ident(t.table_name), 'INSERT') as can_insert, has_table_privilege('{}', quote_ident(t.table_name), 'UPDATE') as can_update, has_table_privilege('{}', quote_ident(t.table_name), 'DELETE') as can_delete, has_table_privilege('{}', quote_ident(t.table_name), 'TRUNCATE') as can_truncate, has_table_privilege('{}', quote_ident(t.table_name), 'REFERENCES') as can_references, has_table_privilege('{}', quote_ident(t.table_name), 'TRIGGER') as can_trigger FROM information_schema.tables t WHERE t.table_schema = 'public' ORDER BY t.table_name;",
            role, role, role, role, role, role, role
        );

        let res = self.sql_executor.execute(&sql, 200).await?;
        let privs = res
            .rows
            .into_iter()
            .filter_map(|row| {
                if row.len() >= 8 {
                    let parse_bool =
                        |s: &str| s.eq_ignore_ascii_case("t") || s.eq_ignore_ascii_case("true");
                    Some(TablePrivilege {
                        table_name: row[0].clone(),
                        schema: "public".into(),
                        select: parse_bool(&row[1]),
                        insert: parse_bool(&row[2]),
                        update: parse_bool(&row[3]),
                        delete: parse_bool(&row[4]),
                        truncate: parse_bool(&row[5]),
                        references: parse_bool(&row[6]),
                        trigger: parse_bool(&row[7]),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(privs)
    }

    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String> {
        Self::validate_identifier(&req.name)?;
        let mut ddl = format!(
            "CREATE ROLE \"{}\" WITH {} {} {} {} CONNECTION LIMIT {}",
            req.name,
            if req.login { "LOGIN" } else { "NOLOGIN" },
            if req.createdb {
                "CREATEDB"
            } else {
                "NOCREATEDB"
            },
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

        if let Some(pwd) = req.password.as_deref() {
            if !pwd.is_empty() {
                let escaped_pwd = pwd.replace('\'', "''");
                ddl.push_str(&format!(" PASSWORD '{}'", escaped_pwd));
            }
        }
        ddl.push(';');

        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn drop_role(&self, name: &str) -> Result<(), String> {
        Self::validate_identifier(name)?;
        let ddl = format!("DROP ROLE \"{}\";", name);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String> {
        Self::validate_identifier(name)?;
        let mut clauses = Vec::new();
        if let Some(login) = req.login {
            clauses.push(if login { "LOGIN" } else { "NOLOGIN" });
        }
        if let Some(createdb) = req.createdb {
            clauses.push(if createdb { "CREATEDB" } else { "NOCREATEDB" });
        }
        if let Some(createrole) = req.createrole {
            clauses.push(if createrole {
                "CREATEROLE"
            } else {
                "NOCREATEROLE"
            });
        }
        if let Some(replication) = req.replication {
            clauses.push(if replication {
                "REPLICATION"
            } else {
                "NOREPLICATION"
            });
        }
        let limit_str;
        if let Some(conn_limit) = req.connection_limit {
            limit_str = format!("CONNECTION LIMIT {}", conn_limit);
            clauses.push(&limit_str);
        }
        let pwd_str;
        if let Some(pwd) = req.password.as_deref() {
            if !pwd.is_empty() {
                let escaped_pwd = pwd.replace('\'', "''");
                pwd_str = format!("PASSWORD '{}'", escaped_pwd);
                clauses.push(&pwd_str);
            }
        }

        if clauses.is_empty() {
            return Ok(());
        }

        let ddl = format!("ALTER ROLE \"{}\" WITH {};", name, clauses.join(" "));
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(group_role)?;
        let ddl = format!("GRANT \"{}\" TO \"{}\";", group_role, role);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(group_role)?;
        let ddl = format!("REVOKE \"{}\" FROM \"{}\";", group_role, role);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn set_table_privilege(
        &self,
        role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(&req.table_name)?;
        let schema = req.schema.as_deref().unwrap_or("public");
        Self::validate_identifier(schema)?;

        let verb = if req.grant { "GRANT" } else { "REVOKE" };
        let preposition = if req.grant { "TO" } else { "FROM" };
        let ddl = format!(
            "{} {} ON TABLE \"{}\".\"{}\" {} \"{}\";",
            verb,
            req.privilege.as_sql_str(),
            schema,
            req.table_name,
            preposition,
            role
        );
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }
}

/// Query parameters for the /users dashboard view.
#[derive(Debug, Default, Deserialize)]
pub struct UsersQuery {
    pub role: Option<String>,
    pub tab: Option<String>,
}

/// GET /users -> Renders database users and role permissions management page.
pub async fn get_users_page(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<UsersQuery>,
) -> Result<Html<String>, StatusCode> {
    let roles = state.user_service.list_roles().await.unwrap_or_default();
    let active_role_name = params
        .role
        .as_deref()
        .or_else(|| roles.first().map(|r| r.rolname.as_str()));
    let active_role = active_role_name.and_then(|name| roles.iter().find(|r| r.rolname == name));
    let active_tab = params.tab.as_deref().unwrap_or("attributes");

    let table_privileges = if let Some(r) = active_role {
        state
            .user_service
            .get_table_privileges(&r.rolname)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let all_tables = api_list_tables(State(state.clone())).await.0;

    let template = UsersTemplate {
        roles: &roles,
        active_role,
        active_tab,
        table_privileges: &table_privileges,
        all_tables: &all_tables,
        all_roles: &roles,
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render users template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /api/users -> List all database roles (excluding system pg_* and postgres roles)
pub async fn api_list_users(State(state): State<Arc<DashboardState>>) -> Json<Vec<PgRole>> {
    let roles = state.user_service.list_roles().await.unwrap_or_default();
    Json(roles)
}

/// GET /api/users/:role -> Get details of a single database role
pub async fn api_get_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<PgRole>, (StatusCode, String)> {
    match state.user_service.get_role(&role).await {
        Ok(Some(r)) => Ok(Json(r)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Role '{}' not found", role))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/users -> Create a new database role
pub async fn api_create_user(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<CreateRoleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("CREATE ROLE \"{}\" WITH LOGIN;", req.name);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .create_role(&req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!(
                "Database role '{}' created (login={}, createdb={}, createrole={})",
                req.name, req.login, req.createdb, req.createrole
            ),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' created successfully", req.name)
    })))
}

/// DELETE /api/users/:role -> Drop an existing database role
pub async fn api_drop_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("DROP ROLE \"{}\";", role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .drop_role(&role)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Database role '{}' dropped", role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' dropped successfully", role)
    })))
}

/// PUT /api/users/:role -> Alter attributes of an existing database role
pub async fn api_alter_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<AlterRoleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("ALTER ROLE \"{}\" WITH LOGIN;", role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .alter_role(&role, &req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Database role '{}' attributes altered", role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' updated successfully", role)
    })))
}

/// POST /api/users/:role/memberships -> Grant group membership to a role
pub async fn api_grant_membership(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<RoleMembershipRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("GRANT \"{}\" TO \"{}\";", req.group_role, role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .grant_membership(&role, &req.group_role)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Granted group '{}' to role '{}'", req.group_role, role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Granted group '{}' to role '{}'", req.group_role, role)
    })))
}

/// DELETE /api/users/:role/memberships/:group -> Revoke group membership from a role
pub async fn api_revoke_membership(
    State(state): State<Arc<DashboardState>>,
    Path((role, group)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("REVOKE \"{}\" FROM \"{}\";", group, role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .revoke_membership(&role, &group)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Revoked group '{}' from role '{}'", group, role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Revoked group '{}' from role '{}'", group, role)
    })))
}

/// GET /api/users/:role/privileges -> List table privileges for a role
pub async fn api_get_privileges(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<Vec<TablePrivilege>>, (StatusCode, String)> {
    let privs = state
        .user_service
        .get_table_privileges(&role)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(privs))
}

/// POST /api/users/:role/privileges -> Grant or revoke a table privilege for a role
pub async fn api_set_privilege(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<TablePrivilegeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let verb = if req.grant { "GRANT" } else { "REVOKE" };
    let prep = if req.grant { "TO" } else { "FROM" };
    let ddl_check = format!(
        "{} {} ON TABLE public.\"{}\" {} \"{}\";",
        verb,
        req.privilege.as_sql_str(),
        req.table_name,
        prep,
        role
    );

    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .set_table_privilege(&role, &req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!(
                "{} table privilege {} on '{}' {} role '{}'",
                if req.grant { "Granted" } else { "Revoked" },
                req.privilege.as_sql_str(),
                req.table_name,
                if req.grant { "to" } else { "from" },
                role
            ),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!(
            "{} {} on {} {} role {}",
            if req.grant { "Granted" } else { "Revoked" },
            req.privilege.as_sql_str(),
            req.table_name,
            if req.grant { "to" } else { "from" },
            role
        )
    })))
}
