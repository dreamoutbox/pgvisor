### If you want to modify the web dashboard, Askama HTML templates, or SQL console security, then check:

- `crates/pgvisor-dashboard/src/models.rs` = Data transfer models for cluster overview, node summaries, SQL query request/response, TableSummary, ColumnInfo, TableDataResponse
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation) and admin token authentication
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`, `TablesTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `sql.html`, `tables.html`)
- `crates/pgvisor-dashboard/src/handlers.rs` = Axum route handlers for HTML pages (`/`, `/nodes`, `/tables`, `/sql`) and `/api/status`, `/api/sql`, `/api/tables` endpoints
- `crates/pgvisor-dashboard/src/lib.rs` = Router assembly and unit tests
- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary
- `crates/pgvisor-proxy/src/executor.rs` = `ProxySqlExecutor` running dashboard queries against live Postgres cluster connections
