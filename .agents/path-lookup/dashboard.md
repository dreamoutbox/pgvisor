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
- `tests/test-users-permissions.sh` = Integration test for role CRUD, memberships, and table privilege grants

### If you want to modify cluster audit logs, dangerous SQL tracking, PITR recovery recommendations, or S3 audit event persistence:

- `crates/pgvisor-core/src/audit.rs` = `AuditEventKind` enum, `AuditEvent` model, `AuditLog` ring buffer with background S3 persistence and startup loader
- `crates/pgvisor-dashboard/src/models.rs` = `AuditEventView`, `AuditListResponse`, `AuditOverviewStats`
- `crates/pgvisor-dashboard/src/templates.rs` = `AuditTemplate` Askama definition
- `crates/pgvisor-dashboard/templates/audit.html` = Audit logs HTML dashboard page with search, filters, PITR timestamps, and responsive badges
- `crates/pgvisor-dashboard/templates/base.html` = Sidebar navigation link and badge style formatting
- `crates/pgvisor-dashboard/src/handlers.rs` = `/audit-logs` and `/api/audit-logs` Axum handlers, and audit event recording on user management actions
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/audit-logs` and `/api/audit-logs`
- `crates/pgvisor-proxy/src/session.rs` = Auditing dangerous SQL (DROP, TRUNCATE, DELETE) and user/permission SQL commands
- `crates/pgvisor-proxy/src/backup.rs` = Auditing backup creation and restore operations
- `crates/pgvisor-proxy/src/cluster.rs` = Auditing cluster switchover operations
- `crates/pgvisor-proxy/src/main.rs` = AuditLog initialization with S3 operator, topology event polling, and sidecar `/control/events` ingestion
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar event recording and `GET /control/events` endpoint for promotion, demotion, and fencing
- `tests/test-audit-logs.sh` = Integration test suite verifying 9 audit assertions end-to-end

### If you want to modify web dashboard charts, metrics telemetry, node resource monitoring, or query rate tracking, then check:

- `crates/pgvisor-core/src/metrics.rs` = Shared metric snapshot models (`ClusterMetricsSnapshot`, `NodeMetrics`, `ProxyMetrics`, `BackupMetrics`, `NodeMetricRole`)
- `crates/pgvisor-sidecar/src/system.rs` = Container `/proc/stat` and `/proc/meminfo` metrics collection for node CPU and memory
- `crates/pgvisor-sidecar/src/supervisor.rs` = Node process uptime tracking (`uptime_secs`)
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar `StatusResponse` exposing CPU, memory, and uptime metrics over `/control/status`
- `crates/pgvisor-dashboard/src/metrics.rs` = `MetricsService` trait, `StandaloneMetricsService`, and `/api/metrics/snapshot`, `/api/metrics/history` handlers
- `crates/pgvisor-dashboard/src/models.rs` = Re-exporting metric snapshot types for dashboard
- `crates/pgvisor-dashboard/src/templates.rs` = `OverviewTemplate` Askama definition
- `crates/pgvisor-dashboard/templates/overview.html` = Integrated Chart.js HTML template with cluster topology and 7 live telemetry charts (uptime, node read/write, CPU, memory, proxy read/write, replication lag, backup throughput/rate)
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/` and `/api/metrics/*`
- `crates/pgvisor-proxy/src/metrics.rs` = `ProxyMetricsStore` atomic counters and `ProxyMetricsService` aggregating rolling history
- `crates/pgvisor-proxy/src/session.rs` = L7 proxy read and write query counting (`ProxyMetricsStore::record_read` / `record_write`)
- `crates/pgvisor-proxy/src/main.rs` = Proxy topology monitor querying replication lag from `pg_stat_replication` and sidecar telemetry
- `tests/test-metrics.sh` = Integration test suite verifying metrics endpoints, proxy query counter increments, node telemetry, and backup metrics

