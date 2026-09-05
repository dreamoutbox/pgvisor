### If you want to modify the web dashboard, Askama HTML templates, or SQL console security, then check:

- `crates/pgvisor-dashboard/src/models.rs` = Data transfer models for cluster overview, node summaries, SQL query request/response, TableSummary, ColumnInfo, TableDataResponse, and backup request/view models
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation) and admin token authentication (`verify_admin_token`, `extract_token_from_headers`)
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`, `TablesTemplate`, `BackupsTemplate`, `LoginTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `sql.html`, `tables.html`, `backups.html`, `login.html`)
- `crates/pgvisor-dashboard/src/handlers.rs` = Axum route handlers for HTML pages (`/`, `/nodes`, `/tables`, `/sql`, `/backups`, `/login`, `/logout`) and `/api/status`, `/api/nodes`, `/api/sql`, `/api/tables`, `/api/backups` endpoints
- `crates/pgvisor-dashboard/src/lib.rs` = Router assembly, `auth_middleware`, and unit tests
- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary
- `crates/pgvisor-proxy/src/executor.rs` = `ProxySqlExecutor` running dashboard queries against live Postgres cluster connections
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService` orchestrating OpenDAL backups and restores for the proxy dashboard
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService` orchestrating manual leader switchover and standby repointing for the proxy dashboard

