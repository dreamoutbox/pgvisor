### If you want to modify the standalone web dashboard UI, Askama HTML templates, or sidecar `/api` HTTP proxy client, then check:

- `crates/pgvisor-dashboard/src/main.rs` = Standalone HTTP server binary reading `PGVISOR_DASHBOARD_LISTEN`, `PGVISOR_SIDECAR_URLS`, `PGVISOR_CLUSTER_ID`, and `PGVISOR_CLUSTER_NAME`
- `crates/pgvisor-dashboard/src/lib.rs` = Crate exports and router wiring for standalone `pgvisor-dashboard`
- `crates/pgvisor-dashboard/src/client/router.rs` = Standalone Axum router mapping SSR pages and `/api/*` BFF endpoints
- `crates/pgvisor-dashboard/src/client/client_dashboard_state.rs` = `ClientDashboardState` holding `SidecarClient`, `cluster_id`, and `cluster_name`
- `crates/pgvisor-dashboard/src/client/sidecar_client.rs` = `SidecarClient` discovering the leader via `GET /api/cluster/members` and retrying `409 Conflict` (`not_leader`) against `leader_url`
- `crates/pgvisor-dashboard/src/client/sidecar_response.rs` = Upstream sidecar HTTP response wrapper and `not_leader` JSON parser
- `crates/pgvisor-dashboard/src/client/auth_handlers.rs` = `/login`, `/logout`, and `pgvisor_token` cookie forwarding
- `crates/pgvisor-dashboard/src/client/page_handlers.rs` = Askama SSR page handlers fetching views from sidecar `/api/*`
- `crates/pgvisor-dashboard/src/client/proxy_handlers.rs` = Transparent `/api/*` JSON/multipart proxy handlers forwarding `Authorization` and cookies to sidecar
- `crates/pgvisor-dashboard/src/templates.rs` = Askama template structs (`OverviewTemplate`, `NodesTemplate`, `SqlConsoleTemplate`, `TablesTemplate`, `BackupsTemplate`, `UsersTemplate`, `AuditTemplate`, `LoginTemplate`)
- `crates/pgvisor-dashboard/templates/` = HTML templates (`base.html`, `overview.html`, `nodes.html`, `node_inspect.html`, `sql.html`, `tables.html`, `backups.html`, `users.html`, `audit.html`, `login.html`)

### If you want to modify sidecar `/api` authentication (`PGVISOR_SUPERUSER`), `pg_authid` password login, or `/api/cluster/members` leader discovery, then check:

- `crates/pgvisor-sidecar/src/api/server.rs` = Sidecar `/api` Axum listener bound to `PGVISOR_API_PORT` (default `8081`), separate from `/control` (`PGVISOR_CONTROL_PORT`, default `8080`)
- `crates/pgvisor-sidecar/src/api/router.rs` = Sidecar `/api` route registration (`auth/login`, `auth/me`, `cluster/members`, `overview`, `nodes/*`, `sql`, `tables/*`, `backups/*`, `users/*`, `audit-logs`, `metrics/*`) and `PGVISOR_SUPERUSER` auth middleware
- `crates/pgvisor-sidecar/src/api/auth.rs` = `POST /api/auth/login` and `GET /api/auth/me` handlers plus `AuthMiddleware` enforcing Bearer/cookie token and `PGVISOR_SUPERUSER`
- `crates/pgvisor-sidecar/src/api/superusers.rs` = `SuperuserAllowlist` parsing `PGVISOR_SUPERUSER` (default `postgres`, returns `403 Forbidden` if user is not listed)
- `crates/pgvisor-sidecar/src/api/password_verifier.rs` = Verifying PostgreSQL passwords against local `pg_authid` SCRAM-SHA-256 / MD5 hashes
- `crates/pgvisor-sidecar/src/api/session_token.rs` = HMAC session token issue/verify logic (`DEFAULT_TOKEN_TTL_SECS`)
- `crates/pgvisor-sidecar/src/api/members.rs` = `GET /api/cluster/members` handler reporting all configured/discovered members, `api_url`, `role`, `health`, `is_self`, and `leader_url`
- `crates/pgvisor-sidecar/src/api/error.rs` = Sidecar API error responses including `409 Conflict` `{"status": "not_leader", "leader_url": ...}`
- `crates/pgvisor-core/src/api/` = Shared API request/response DTOs (`login_request.rs`, `login_response.rs`, `me_response.rs`, `cluster_members_response.rs`, `cluster_member.rs`, `sidecar_role.rs`, `member_health.rs`)

### If you want to modify SQL console security, table explorer, row editing/deletion, or `/api/users/*` PostgreSQL role management, then check:

- `crates/pgvisor-core/src/sql_guard/sql_security_guard.rs` = `SqlSecurityGuard` (comment stripping, multi-statement rejection, read-only validation, `validate_user_management_ddl`)
- `crates/pgvisor-core/src/api/` = `sql_query_request.rs`, `sql_query_result.rs`, `table_summary.rs`, `column_info.rs`, `table_data_response.rs`, `delete_row_request.rs`, `update_row_request.rs`, `row_mutation_response.rs`, `pg_role.rs`, `table_privilege.rs`, `table_privilege_kind.rs`
- `crates/pgvisor-sidecar/src/api/sql.rs` = `/api/sql`, `/api/databases`, `/api/tables`, `/api/tables/:table/data`, `DELETE/PUT /api/tables/:table/rows` handlers, primary key lookup, and row mutation SQL builders
- `crates/pgvisor-sidecar/src/api/sql_runner.rs` = Executing SQL queries against local/leader PostgreSQL via `pg_client`
- `crates/pgvisor-sidecar/src/api/users.rs` = `/api/users` role CRUD, memberships, and table privilege management handlers
- `crates/pgvisor-dashboard/templates/tables.html` = Data Browse view with database selector, no-PK warning, Delete/Edit Row Modals, and AJAX handlers
- `crates/pgvisor-dashboard/templates/users.html` = Users & roles management UI with attributes, memberships, and privileges tabs
- `tests/test-users-permissions.sh` = Integration test for role CRUD, memberships, and table privilege grants
- `tests/test-dashboard-tables-nodes.sh` = Integration test verifying row editing, row deletion, and audit logging

### If you want to inspect node PostgreSQL logs, config files, or start/stop/restart/switchover nodes from the sidecar API or web dashboard, then check:

- `crates/pgvisor-core/src/node.rs` = `NodeConfigType`, `NodeLogEntry`, `NodeLogsResponse`, `NodeConfigResponse`
- `crates/pgvisor-core/src/api/` = `node_summary.rs`, `node_health_state.rs`, `node_lifecycle_action.rs`, `node_action_request.rs`, `node_action_response.rs`, `switchover_request.rs`, `switchover_response.rs`
- `crates/pgvisor-sidecar/src/api/cluster.rs` = `/api/overview`, `/api/nodes`, `/api/nodes/:node_id/logs`, `/api/nodes/:node_id/config/:config_type`
- `crates/pgvisor-sidecar/src/api/peer_probe.rs` = Peer `/control/status` probing and dynamic `pg_stat_replication` peer discovery (`discover_replication_hosts`, `infer_control_port`)
- `crates/pgvisor-sidecar/src/api/cluster_actions.rs` = Leader-only `/api/cluster/switchover` and `/api/nodes/:node_id/{start,stop,restart,action}` orchestrating sidecar `/control/*` calls
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Internal sidecar `/control/logs`, `/control/config/:config_type`, `/control/start`, `/control/stop`, `/control/restart`
- `crates/pgvisor-dashboard/templates/nodes.html` = Nodes table with start/stop/restart actions, leader stop warning modal, and Inspect link
- `crates/pgvisor-dashboard/templates/node_inspect.html` = Live server logs and config/diagnostic file viewer
- `tests/test-node-lifecycle.sh` = Integration test verifying end-to-end node stop, start, restart, pool continuity, and error cases

### If you want to modify cluster audit logs or S3 audit event persistence, then check:

- `crates/pgvisor-core/src/audit.rs` = `AuditEventKind`, `AuditEvent`, `AuditLog` ring buffer with OpenDAL S3 persistence (`MAX_EVENTS_PER_BATCH`, `MAX_BATCH_BYTES`)
- `crates/pgvisor-core/src/api/` = `audit_event_view.rs`, `audit_list_response.rs`, `audit_overview_stats.rs`
- `crates/pgvisor-sidecar/src/api/api_state.rs` = `AuditLog` initialization with S3 operator and background sidecar `/control/events` ingestion
- `crates/pgvisor-sidecar/src/api/audit.rs` = `/api/audit-logs` Axum handler, pagination computation, and audit views
- `crates/pgvisor-sidecar/src/api/{sql,users,cluster_actions}.rs` and `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = Recording audit events on SQL mutations, user changes, switchovers, and backup/restore operations
- `crates/pgvisor-dashboard/src/templates.rs` = `AuditTemplate` with `page_items`, `start_item`, `end_item`
- `crates/pgvisor-dashboard/templates/audit.html` = Audit logs HTML template with search, filters, PITR hint box, and pagination
- `tests/test-audit-logs.sh` = Integration test suite verifying audit assertions end-to-end

### If you want to modify telemetry metrics, node resource monitoring, or PostgreSQL replication/activity stats, then check:

- `crates/pgvisor-core/src/metrics.rs` = `ClusterMetricsSnapshot`, `NodeMetrics`, `DatabaseMetrics`, `BackupMetrics`, `NodeMetricRole`
- `crates/pgvisor-sidecar/src/system.rs` = Container `/proc/stat` and `/proc/meminfo` metrics collection for node CPU and memory
- `crates/pgvisor-sidecar/src/supervisor.rs` = Node process uptime tracking (`uptime_secs`)
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Sidecar `/control/status` exposing CPU, memory, and uptime metrics
- `crates/pgvisor-sidecar/src/api/pg_activity.rs` = Polling `pg_stat_replication` lag and `pg_stat_activity` / `pg_stat_database` activity metrics
- `crates/pgvisor-sidecar/src/api/metrics.rs` = Rolling history store and `/api/metrics/snapshot`, `/api/metrics/history` handlers
- `crates/pgvisor-dashboard/templates/overview.html` = Chart.js overview dashboard displaying topology and live metrics charts
- `tests/test-metrics.sh` = Integration test suite verifying metrics endpoints, node telemetry, and backup metrics
