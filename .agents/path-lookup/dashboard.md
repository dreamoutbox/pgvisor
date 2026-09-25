### If you want to modify the web dashboard, Askama HTML templates, or SQL console security, then check:

- `crates/pgvisor-dashboard/src/models.rs` = Data transfer models for cluster overview, node summaries, SQL query request/response, TableSummary, ColumnInfo, TableDataResponse, backup models, PgRole, TablePrivilege, TablePrivilegeKind, and user management request models
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation, `validate_user_management_ddl`) and admin token authentication (`verify_admin_token`, `extract_token_from_headers`)
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`, `TablesTemplate`, `BackupsTemplate`, `UsersTemplate`, `LoginTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `sql.html`, `tables.html`, `backups.html`, `users.html`, `login.html`)
- `crates/pgvisor-dashboard/src/handlers/mod.rs` = Sparse module root with explicit re-exports of all dashboard handler services and endpoints
- `crates/pgvisor-dashboard/src/handlers/state.rs` = `DashboardState` struct and initialization
- `crates/pgvisor-dashboard/src/handlers/sql.rs` = `SqlExecutor` trait, `StandaloneSqlExecutor`, `TablesQuery`, and SQL explorer / table schema endpoints
- `crates/pgvisor-dashboard/src/handlers/backup.rs` = `BackupService` trait, `StandaloneBackupService`, snapshot selection, and backup / PITR restore endpoints
- `crates/pgvisor-dashboard/src/handlers/cluster.rs` = `ClusterService` trait (`get_node_logs`, `get_node_config`), `StandaloneClusterService`, `/api/nodes/:node_id/logs`, and `/api/nodes/:node_id/config/:config_type` endpoints
- `crates/pgvisor-dashboard/src/handlers/users.rs` = `UserService` trait, `StandaloneUserService`, `SqlUserService`, and role / permission management endpoints
- `crates/pgvisor-dashboard/src/handlers/auth.rs` = Login, logout, and token session cookie handlers
- `crates/pgvisor-dashboard/src/handlers/audit.rs` = Audit log page and API list handlers
- `crates/pgvisor-dashboard/src/lib.rs` = Router assembly, `auth_middleware`, and unit tests
- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary
- `crates/pgvisor-proxy/src/main.rs` = Proxy server entrypoint, injects `ProxySqlExecutor`, `ProxyBackupService`, `ProxyClusterService`, and `SqlUserService` into dashboard state
- `crates/pgvisor-proxy/src/executor.rs` = `ProxySqlExecutor` running dashboard queries against live Postgres cluster connections
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService` orchestrating OpenDAL backups and restores for the proxy dashboard
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService` orchestrating manual leader switchover, standby repointing, node logs forwarding, and config inspection for the proxy dashboard

### If you want to inspect node PostgreSQL logs, postgresql.conf, pg_hba.conf, or runtime diagnostic files in the web dashboard:

- `crates/pgvisor-core/src/node.rs` = `NodeConfigType` enum (`PostgresqlConf`, `PostgresqlAutoConf`, `PgHbaConf`, `PgIdentConf`, `PostmasterPid`, `PostmasterOpts`, `StandbySignal`, `RecoverySignal`, `BackupLabel`), `NodeLogEntry`, and DTO models
- `crates/pgvisor-dashboard/src/handlers/cluster.rs` = `ClusterService` trait (`get_node_logs`, `get_node_config`), `api_node_logs`, and `api_node_config` endpoints
- `crates/pgvisor-dashboard/templates/nodes.html` = Inspect button on node rows, modal tabs for live log streaming / search / auto-refresh, and diagnostic file viewer
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService::get_node_logs` and `get_node_config` proxying requests to sidecars via HTTP auth

### If you want to manage database users, roles, memberships, or table privileges:

- `crates/pgvisor-dashboard/src/models.rs` = `PgRole`, `TablePrivilege`, `TablePrivilegeKind`, and user management request models
- `crates/pgvisor-dashboard/src/security.rs` = `SqlSecurityGuard::validate_user_management_ddl` allow-list for role DDL and GRANT/REVOKE
- `crates/pgvisor-dashboard/src/handlers/users.rs` = `UserService` trait, `StandaloneUserService`, `SqlUserService`, and `/api/users/*` handlers
- `crates/pgvisor-dashboard/src/templates.rs` = `UsersTemplate` Askama definition
- `crates/pgvisor-dashboard/templates/users.html` = Users & roles management HTML template with attributes, memberships, and privileges tabs
- `crates/pgvisor-dashboard/templates/base.html` = Sidebar navigation link for `Users & Roles`
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/users` and `/api/users/*`
- `tests/test-users-permissions.sh` = Integration test for role CRUD, memberships, and table privilege grants

### If you want to modify cluster audit logs, dangerous SQL tracking, PITR recovery recommendations, or S3 audit event persistence:

- `crates/pgvisor-core/src/audit.rs` = `AuditEventKind` enum, `AuditEvent` model, `AuditLog` ring buffer with multi-event JSON batch persistence (`MAX_EVENTS_PER_BATCH`, `MAX_BATCH_BYTES`), size/count rotation, and backward-compatible startup loader
- `crates/pgvisor-dashboard/src/models.rs` = `AuditEventView`, `AuditListResponse`, `AuditOverviewStats`
- `crates/pgvisor-dashboard/src/templates.rs` = `PageItem` struct and `AuditTemplate` Askama definition with `page_items`, `start_item`, `end_item`
- `crates/pgvisor-dashboard/templates/audit.html` = Audit logs HTML dashboard page with search, filters, compact table density (`.audit-table`), compact badges and PITR box, rows-per-page selector, and full pagination navigation
- `crates/pgvisor-dashboard/templates/base.html` = Sidebar navigation link and badge style formatting
- `crates/pgvisor-dashboard/src/handlers/audit.rs` = `/audit-logs` and `/api/audit-logs` Axum handlers, pagination item computation, and audit event views
- `crates/pgvisor-dashboard/src/handlers/users.rs` = Audit event recording on role creation, modification, and privilege changes
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/audit-logs` and `/api/audit-logs`
- `crates/pgvisor-proxy/src/session.rs` = Auditing dangerous SQL (DROP, TRUNCATE, DELETE) and user/permission SQL commands
- `crates/pgvisor-proxy/src/backup.rs` = Auditing backup creation and restore operations
- `crates/pgvisor-proxy/src/cluster.rs` = Auditing cluster switchover operations
- `crates/pgvisor-proxy/src/main.rs` = AuditLog initialization with S3 operator, topology event polling, and sidecar `/control/events` ingestion
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Sidecar event recording and `GET /control/events` endpoint for promotion, demotion, and fencing
- `tests/test-audit-logs.sh` = Integration test suite verifying 9 audit assertions end-to-end

### If you want to modify web dashboard charts, metrics telemetry, node resource monitoring, or query rate tracking, then check:

- `crates/pgvisor-core/src/metrics.rs` = Shared metric snapshot models (`ClusterMetricsSnapshot`, `NodeMetrics`, `ProxyMetrics`, `BackupMetrics`, `NodeMetricRole`)
- `crates/pgvisor-sidecar/src/system.rs` = Container `/proc/stat` and `/proc/meminfo` metrics collection for node CPU and memory
- `crates/pgvisor-sidecar/src/supervisor.rs` = Node process uptime tracking (`uptime_secs`)
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Sidecar `StatusResponse` exposing CPU, memory, and uptime metrics over `/control/status`
- `crates/pgvisor-dashboard/src/metrics.rs` = `MetricsService` trait, `StandaloneMetricsService`, and `/api/metrics/snapshot`, `/api/metrics/history` handlers
- `crates/pgvisor-dashboard/src/models.rs` = Re-exporting metric snapshot types for dashboard
- `crates/pgvisor-dashboard/src/templates.rs` = `OverviewTemplate` Askama definition
- `crates/pgvisor-dashboard/templates/overview.html` = Integrated Chart.js HTML template with cluster topology and 7 live telemetry charts (uptime, node read/write, CPU, memory, proxy read/write, replication lag, backup throughput/rate)
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration for `/` and `/api/metrics/*`
- `crates/pgvisor-proxy/src/metrics.rs` = `ProxyMetricsStore` atomic counters and `ProxyMetricsService` aggregating rolling history
- `crates/pgvisor-proxy/src/session.rs` = L7 proxy read and write query counting (`ProxyMetricsStore::record_read` / `record_write`)
- `crates/pgvisor-proxy/src/main.rs` = Proxy topology monitor querying replication lag from `pg_stat_replication` and sidecar telemetry
- `tests/test-metrics.sh` = Integration test suite verifying metrics endpoints, proxy query counter increments, node telemetry, and backup metrics

### If you want to start, stop, or restart cluster nodes from the web dashboard or API:

- `crates/pgvisor-dashboard/src/models.rs` = `NodeHealthState::Stopped`, `NodeLifecycleAction` enum, `NodeActionRequest`, and `NodeActionResponse`
- `crates/pgvisor-dashboard/src/handlers/cluster.rs` = `ClusterService` trait (`start_node`, `stop_node`, `restart_node`), `StandaloneClusterService`, and `/api/nodes/:node_id/{start,stop,restart,action}` handlers
- `crates/pgvisor-dashboard/src/lib.rs` = Route registration and unit tests for node lifecycle
- `crates/pgvisor-dashboard/templates/nodes.html` = Nodes page table actions, start/stop/restart buttons, leader stop warning modal, and fetch API invocation
- `crates/pgvisor-dashboard/templates/overview.html` = Overview status badge for `NodeHealthState::Stopped`
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService` orchestrating node start/stop/restart via sidecar HTTP API, pool draining, and audit logging
- `crates/pgvisor-proxy/src/main.rs` = Topology heartbeat mapping sidecar `"stopped"` status to `NodeHealthState::Stopped`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing process start, stop, and restart with status verification
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Sidecar `/control/start`, `/control/stop`, `/control/restart` endpoints
- `crates/pgvisor-sidecar/src/election.rs` = Election monitor pause on stopped status and fenced auto-rejoin
- `tests/test-node-lifecycle.sh` = Integration test verifying end-to-end node stop, start, restart, pool continuity, error cases, and audit logs
