# Proxy ↔ Sidecar Communication

The proxy (`pgvisor-proxy`) never talks to the PostgreSQL instances on the sidecar
nodes via an internal RPC or message bus. Instead, every sidecar node exposes a
lightweight **HTTP control API** (default port `8080`, overridable via
`PGVISOR_CONTROL_PORT`). The proxy calls this API with plain JSON over HTTP.

---

## Sidecar Control API surface

Defined and served inside
[`crates/pgvisor-sidecar/src/main.rs`](../crates/pgvisor-sidecar/src/main.rs)
at startup:

| Method | Path | Handler |
|--------|------|---------|
| `GET`  | `/control/status`  | `handle_status` – returns `node_id`, `role`, `status`, `child_pid`, `pg_version` |
| `GET`  | `/control/events`  | `handle_events` – auditable lifecycle event log (polling) |
| `POST` | `/control/start`   | `handle_start` – spawns PostgreSQL child process under sidecar supervision |
| `POST` | `/control/stop`    | `handle_stop` – `pg_ctl stop -m fast` graceful shutdown |
| `POST` | `/control/restart` | `handle_restart` – stops and restarts PostgreSQL under supervision |
| `POST` | `/control/promote` | `handle_promote` – `pg_ctl promote` → role becomes `leader` |
| `POST` | `/control/fence`   | `handle_fence` – `pg_ctl stop -m immediate` → role becomes `fenced` |
| `POST` | `/control/demote`  | `handle_demote` – graceful stop → role becomes `fenced` |
| `POST` | `/control/repoint` | `handle_repoint` – updates `primary_conninfo`, restarts replica pointing to new leader |
| `POST` | `/control/restore` | `handle_restore` – downloads a base-backup snapshot and restores PGDATA |
| `POST` | `/control/resync`  | `handle_resync` – `pg_basebackup` re-clone from primary |

The proxy builds the control URL by extracting the hostname from the
PostgreSQL address and substituting port 8080:

```rust
// crates/pgvisor-proxy/src/main.rs
let control_url = format!("http://{}:{}", host, control_port);
```

---

## Topology heartbeat loop (500 ms)

The proxy runs a background task
([`main.rs` L234–L585](../crates/pgvisor-proxy/src/main.rs)) that polls
`GET /control/status` on every known node every 500 ms. This is how topology
changes are detected dynamically — there is no push mechanism from sidecar to
proxy.

```
proxy (every 500 ms)
  │
  ├─► GET http://node1:8080/control/status  →  { role: "leader",  status: "running" }
  ├─► GET http://node2:8080/control/status  →  { role: "standby", status: "running" }
  └─► GET http://node3:8080/control/status  →  { role: "standby", status: "fenced"  }
      │
      └─ proxy updates ConnectionPool topology:
           leader_ref  = "node1:5432"
           standby_ref = ["node2:5432"]
           (node3 excluded because fenced)
```

---

## Scenario 1 — Update node config (repoint / resync)

This is triggered during a **switchover** or **failover**, not through a
"config update" API. The proxy never writes `postgresql.conf` directly.
Instead it calls the relevant control endpoint:

### Manual switchover (dashboard-initiated)

Implemented in
[`crates/pgvisor-proxy/src/cluster.rs`](../crates/pgvisor-proxy/src/cluster.rs)
→ `ProxyClusterService::switchover`.

```
Dashboard user clicks "Switchover to node 2"
  │
  ▼
POST /api/cluster/switchover  (dashboard HTTP)
  │
  ▼ ProxyClusterService::switchover (proxy in-process)
  │
  ├─ 1. GET  http://node1:8080/control/status  →  verify node1 is current leader
  │         GET  http://node2:8080/control/status  →  verify node2 is running standby
  │
  ├─ 2. POST http://node1:8080/control/demote  →  node1 stops Postgres gracefully,
  │                                                 role becomes "fenced"
  │
  ├─ 3. POST http://node2:8080/control/promote →  node2 runs `pg_ctl promote`,
  │                                                 role becomes "leader"
  │
  ├─ 4. POST http://node3:8080/control/repoint →  node3 gets new primary_conninfo
  │         body: { "primary_conninfo": "host=node2 port=5432 user=postgres" }
  │         sidecar writes new postgresql.conf + restarts replica
  │
  └─ 5. proxy drains ConnectionPool → updates topology:
           leader_ref  = "node2:5432"
           standby_ref = ["node3:5432"]
```

### Auto-failover (sidecar-driven election, no proxy involvement)

Each sidecar runs its own heartbeat loop and may self-elect. After promoting,
it calls `POST /control/repoint` on peer sidecars directly — the proxy picks
up the new topology on the next 500 ms poll.

---

## Scenario 2 — Execute SQL from the web dashboard

The dashboard is embedded inside the proxy process. SQL execution goes through
an in-process call chain — **no HTTP hop to the sidecar is involved**.

```
Browser user types SQL in dashboard console
  │
  ▼
POST /api/sql  (axum HTTP handler in proxy process)
  │
  ▼ api_execute_sql  [crates/pgvisor-dashboard/src/handlers.rs]
  │   1. SqlSecurityGuard::validate_sql() — rejects mutations when read-only mode is on,
  │      blocks multi-statement scripts, strips comments.
  │   2. tokio::time::timeout(max_execution_timeout, state.sql_executor.execute(sql))
  │
  ▼ ProxySqlExecutor::execute  [crates/pgvisor-proxy/src/executor.rs]
  │   3. is_leader_required(sql)?
  │        YES (INSERT/UPDATE/DELETE/DDL/multi-stmt) → BackendRole::Leader
  │        NO  (SELECT, SHOW, EXPLAIN)               → BackendRole::Standby
  │   4. ConnectionPool::acquire_with_retry(role) → picks an idle TCP connection
  │      from the pool (already authenticated via Postgres wire protocol startup)
  │   5. Sends Query message (PostgreSQL wire protocol, simple query flow)
  │   6. Reads RowDescription / DataRow / CommandComplete / ErrorResponse frames
  │   7. Releases connection back to pool
  │
  ▼ SqlQueryResult { columns, rows, row_count, execution_time_ms, truncated }
  │
  ▼ JSON response to browser
```

Key points:

- **No sidecar HTTP call** for SQL execution. The proxy already holds live
  Postgres wire-protocol connections in its `ConnectionPool`.
- **Read/write splitting** happens transparently: `SELECT` → standby,
  mutations → leader. This means post-DDL `SELECT` verifications can hit
  stale replicas (replication lag). Use a retry loop in tests.
- The `SqlSecurityGuard` wraps the executor at the dashboard layer and can
  enforce read-only mode independently of the pool routing.

---

## Component map

```
crates/pgvisor-proxy/src/
  main.rs       — topology heartbeat loop, wires ProxySqlExecutor + ProxyClusterService
                  into DashboardState; spawns dashboard listener
  cluster.rs    — ProxyClusterService: switchover via sidecar HTTP control API
  executor.rs   — ProxySqlExecutor: SQL over ConnectionPool (no sidecar involved)
  pool.rs       — ConnectionPool: BackendRole-aware idle connection store

crates/pgvisor-sidecar/src/
  main.rs       — HTTP control server: /control/* routes + election heartbeat loop
  config.rs     — PostgresConfig → writes postgresql.conf + pg_hba.conf to PGDATA
  supervisor.rs — PostgresSupervisor: promote / fence / stop / repoint / resync

crates/pgvisor-dashboard/src/
  handlers.rs   — api_execute_sql, DashboardState, SqlExecutor trait
  security.rs   — SqlSecurityGuard (read-only gate, comment stripping, timeout)
```
