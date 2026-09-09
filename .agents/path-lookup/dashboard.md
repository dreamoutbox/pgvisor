### If you want to modify the web dashboard, Askama HTML templates, or SQL console security, then check:

- `crates/pgvisor-dashboard/src/models.rs` = Data transfer models for cluster overview, node summaries, SQL query request/response, TableSummary, ColumnInfo, TableDataResponse, backup models, PgRole, TablePrivilege, TablePrivilegeKind, and user management request models
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation, `validate_user_management_ddl`) and admin token authentication (`verify_admin_token`, `extract_token_from_headers`)
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`, `TablesTemplate`, `BackupsTemplate`, `UsersTemplate`, `LoginTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `sql.html`, `tables.html`, `backups.html`, `users.html`, `login.html`)
- `crates/pgvisor-dashboard/src/handlers.rs` = Axum route handlers for HTML pages (`/`, `/nodes`, `/tables`, `/sql`, `/backups`, `/users`, `/login`, `/logout`), `UserService` trait + `StandaloneUserService` + `SqlUserService`, and `/api/*` endpoints
- `crates/pgvisor-dashboard/src/lib.rs` = Router assembly, `auth_middleware`, and unit tests
- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary
- `crates/pgvisor-proxy/src/main.rs` = Proxy server entrypoint, injects `ProxySqlExecutor`, `ProxyBackupService`, `ProxyClusterService`, and `SqlUserService` into dashboard state
- `crates/pgvisor-proxy/src/executor.rs` = `ProxySqlExecutor` running dashboard queries against live Postgres cluster connections
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService` orchestrating OpenDAL backups and restores for the proxy dashboard
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService` orchestrating manual leader switchover and standby repointing for the proxy dashboard

### If you want to manage database users, roles, memberships, or table privileges:

- `crates/pgvisor-dashboard/src/models.rs` = `PgRole`, `TablePrivilege`, `TablePrivilegeKind`, and user management request models
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard::validate_user_management_ddl` allow-list for role DDL and GRANT/REVOKE
- `crates/pgvisor-dashboard/src/handlers.rs` = `UserService` trait, `StandaloneUserService`, `SqlUserService`, and `/api/users/*` handlers
- `crates/pgvisor-dashboard/src/templates.rs` = `UsersTemplate` Askama definition
- `crates/pgvisor-dashboard/templates/users.html` = Users & roles management HTML template with attributes, memberships, and privileges tabs
- `crates/pgvisor-dashboard/templates/base.html` = Sidebar navigation link for `Users & Roles`
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/users` and `/api/users/*`
- `crates/pgvisor-proxy/src/main.rs` = Injects `SqlUserService` into proxy dashboard state
- `tests/test-users-permissions.sh` = Integration test for role CRUD, memberships, and table privilege grants

