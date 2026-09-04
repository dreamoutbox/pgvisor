### If you want to modify the web dashboard, Askama HTML templates, or SQL console security, then check:

- `crates/pgvisor-dashboard/src/models.rs` = Data transfer models for cluster overview, node summaries, SQL query request/response
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation) and admin token authentication
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `sql.html`)
- `crates/pgvisor-dashboard/src/handlers.rs` = Axum route handlers for HTML pages and `/api/status`, `/api/sql` endpoints
- `crates/pgvisor-dashboard/src/lib.rs` = Router assembly and unit tests
- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary
