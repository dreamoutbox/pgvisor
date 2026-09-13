# Post-Mortem: Multi-Timeline Divergence, Recovery Overrun, and Total Cluster Failure

## Incident Summary

| Metric | Value |
|---|---|
| **Symptom** | Cluster entered total failure (`Healthy Nodes = 0`), Web Dashboard toast showed `Leader sidecar restore returned error 500 Internal Server Error: {"error":"Failed to restore from snapshot: Command execution failed: Postgres process exited unexpectedly with status: exit status: 1"}`, and `http://localhost:8080/tables` hung indefinitely loading |
| **Duration / Impact** | Total cluster outage: Leader (Node 1) failed startup during archive recovery; Standby nodes (Nodes 2 & 3) crashed on timeline divergence; Web Dashboard SQL explorer hung for 30s |
| **Reproduction** | 1. Restore full backup `f1` with PITR target timestamp `2026-09-13 19:40:09` (forks to Timeline 2).<br>2. Restore incremental backup `incr2` without target timestamp (checkpoint on Timeline 1).<br>3. Re-restore full backup `f1` with PITR target timestamp `2026-09-13 19:40:09`. |
| **Impacted Subsystems** | `pgvisor-sidecar` (`config.rs`, `supervisor.rs`), `pgvisor-proxy` (`backup.rs`, `executor.rs`, `main.rs`), and PostgreSQL recovery engine |
| **Root Causes** | (1) Target timestamp overrun past the last archived WAL record (`19:40:08.844` vs `19:40:09`), (2) Standby replicas attempting to follow divergent timeline branches (`00000002.history`) via `restore_command` without `recovery_target_timeline = 'current'`, (3) Proxy restore handler falling back to `127.0.0.1:8080` (itself) when leader was offline and following HTTP 303 redirect to `/login`, (4) ProxySqlExecutor 30s connection pool retry timeout hanging web dashboard pages |
| **Resolution** | Configured `recovery_target_timeline = 'current'` for standby streaming and targeted PITR recovery; added `configured_leader` fallback to `ProxyBackupService`; disabled HTTP redirect following on sidecar restore clients; reduced `ProxySqlExecutor` failover timeout to 3s |

---

## Detailed Sequence of Failure

```
[User triggers Step 1: Restore f1 with PITR target="2026-09-13 19:40:09"]
                                    │
                                    ▼
1. Node 1 restores f1, replays WAL to 19:40:08.844 ('foxtrot'), stops before 'golf',
   promotes, and forks to TIMELINE 2 (writes 00000002.history to MinIO archive).
   Nodes 2 & 3 re-synced via pg_basebackup from Node 1 (now on Timeline 2).
                                    │
                                    ▼
[User triggers Step 2: Restore incr2 without target time]
                                    │
                                    ▼
2. incr2 was captured on TIMELINE 1 (checkpoint at 0/B000080, after Timeline 2 forked at 0/60094C8).
   Node 1 restores incr2 and runs on Timeline 1.
   Node 2 & 3 re-sync from Node 1.
   BUG: Node 2 & 3 have restore_command configured without recovery_target_timeline.
   PostgreSQL defaults to recovery_target_timeline = 'latest'.
   Standby downloads 00000002.history from MinIO and attempts to switch to Timeline 2!
   FATAL: requested timeline 2 is not a child of this server's history
   Nodes 2 & 3 crash and exit immediately (PID 366, exit code 1).
                                    │
                                    ▼
[User triggers Step 3: Re-restore f1 with PITR target="2026-09-13 19:40:09"]
                                    │
                                    ▼
3. Node 1 untars f1 (Timeline 1 baseline).
   PostgreSQL archive recovery finds 00000002.history in MinIO and defaults to Timeline 2.
   On Timeline 2, the last transaction committed at 19:40:08.844063+00.
   Target time requested is 19:40:09 (156ms past the end of Timeline 2).
   PostgreSQL reaches end of WAL on Timeline 2 without reaching target:
   FATAL: recovery ended before configured recovery target was reached
   Node 1 crashes and exits with exit code 1.
                                    │
                                    ▼
4. ALL THREE PostgreSQL instances are now dead (Healthy Nodes = 0).
                                    │
                                    ▼
5. User navigates to http://localhost:8080/tables:
   BUG: ProxySqlExecutor attempts pool.acquire_with_retry() with 30s timeout.
   Browser tab hangs loading for 30 seconds.
                                    │
                                    ▼
6. User attempts another restore from Dashboard:
   BUG: Because leader_addr is None, ProxyBackupService resolves leader host to 127.0.0.1:8080!
   Proxy sends POST /control/restore to ITSELF.
   Dashboard auth middleware returns HTTP 303 See Other -> /login.
   reqwest::Client follows redirect, gets HTTP 200 OK from /login,
   falsely assumes leader restored, and triggers resync on dead standbys!
```

---

## Root Causes Analysis

### 1. The Recovery Target Overrun (`FATAL: recovery ended before configured recovery target was reached`)
In PostgreSQL 12+, specifying `recovery_target_time` is a strict contract. If the recovery engine finishes replaying all available WAL logs from the archive without reaching a transaction whose commit timestamp is $\ge$ the target timestamp, it terminates with:
```text
LOG: last completed transaction was at log time 2026-09-13 19:40:08.844063+00
FATAL: recovery ended before configured recovery target was reached
LOG: startup process (PID 704) exited with exit code 1
```

In this incident, the user specified `2026-09-13 19:40:09`, which was 156 milliseconds after the last transaction committed on that timeline (`19:40:08.844063+00`). PostgreSQL considered this an incomplete WAL stream and shut down.

### 2. Standby Timeline Divergence Across Repeated Restores
When a cluster performs PITR, PostgreSQL branches to a new timeline (e.g., Timeline 2). A subsequent restore of an earlier backup taken on Timeline 1 creates a timeline divergence in the shared MinIO archive:
- MinIO contains WAL segments from Timeline 1, plus `00000002.history`.
- When standby replicas (Nodes 2 and 3) were re-synced, their `postgresql.conf` included `restore_command`.
- Without an explicit `recovery_target_timeline` setting, PostgreSQL defaults to `'latest'`.
- The standbys downloaded `00000002.history` and attempted to recover into Timeline 2.
- However, their cloned checkpoint was on Timeline 1 at an LSN past the fork point of Timeline 2:
  ```text
  FATAL: requested timeline 2 is not a child of this server's history
  DETAIL: Latest checkpoint in file "backup_label" is at 0/B000080 on timeline 1, but in the history of the requested timeline, the server forked off from that timeline at 0/60094C8.
  ```
- Both standbys exited fatally with status 1.

### 3. Proxy Self-Targeting and HTTP Redirect Follow Bug
In [`crates/pgvisor-proxy/src/backup.rs`](../crates/pgvisor-proxy/src/backup.rs):
```rust
// Flawed logic:
let leader = self.leader_addr.read().await.clone();
let leader_host = leader
    .as_deref()
    .map(|addr| addr.split(':').next().unwrap_or("127.0.0.1"))
    .unwrap_or("127.0.0.1")
    .to_string();
```
When Node 1 failed to start, the proxy discovery monitor cleared `leader_addr` to `None`.
The restore handler defaulted to `"127.0.0.1"`, constructing `http://127.0.0.1:8080/control/restore`. Inside the proxy container, port 8080 is the dashboard itself. The dashboard issued an unauthenticated redirect (`HTTP 303 -> /login`). The default `reqwest::Client` followed the redirect and received `HTTP 200 OK` from `/login`, masking the failure and triggering replica re-sync against an offline leader.

### 4. 30-Second Connection Pool Retry in Dashboard Web Handlers
In [`crates/pgvisor-proxy/src/executor.rs`](../crates/pgvisor-proxy/src/executor.rs), `ProxySqlExecutor` was initialized with `FailoverConfig::default()`, which specifies a 30-second `failover_timeout`. When all 3 database nodes were dead (`Healthy Nodes = 0`), web requests to `GET /tables` blocked for the entire 30 seconds trying to acquire a connection from the dead pool before returning.

---

## Preventative Measures & Fixes

### 1. Pinned `recovery_target_timeline = 'current'`
In [`crates/pgvisor-sidecar/src/config.rs`](../crates/pgvisor-sidecar/src/config.rs):
- **Standby Configuration**: Added `recovery_target_timeline = 'current'` whenever `primary_conninfo` is present. Replicas now strictly track the timeline of the primary they cloned from, ignoring historical timeline branches in MinIO.
- **Targeted PITR Configuration**: Added `recovery_target_timeline = 'current'` whenever `recovery_target_time` is specified. PostgreSQL now recovers strictly along the timeline of the basebackup being restored.

```rust
if let Some(target_time) = &config.recovery_target_time {
    conf_content.push_str(&format!("recovery_target_time = '{target_time}'\n"));
    conf_content.push_str("recovery_target_timeline = 'current'\n");
}

if let Some(primary_info) = &config.primary_conninfo {
    conf_content.push_str(&format!("primary_conninfo = '{primary_info}'\n"));
    conf_content.push_str("recovery_target_timeline = 'current'\n");
}
```

### 2. Configured Leader Fallback in Proxy Restore Service
In [`crates/pgvisor-proxy/src/backup.rs`](../crates/pgvisor-proxy/src/backup.rs):
- `ProxyBackupService` now retains `configured_leader: Option<String>` passed at proxy startup.
- If `leader_addr` is `None` (during an active leader outage), the restore handler falls back to `configured_leader` (`pgvisor-node1:5432`), ensuring the restore command reaches the actual node sidecar.
- Replaced the HTTP client with a non-redirecting client (`reqwest::redirect::Policy::none()`) so 3xx redirects are never interpreted as successful restore responses.

### 3. Fast-Fail Dashboard SQL Executor Timeout
In [`crates/pgvisor-proxy/src/executor.rs`](../crates/pgvisor-proxy/src/executor.rs):
- Reduced `failover_timeout` for `ProxySqlExecutor` from 30s to 3s. If all nodes are offline, web UI pages fail fast in 3 seconds instead of hanging the browser.

---

## Verification

The fixes were validated through the exact reproduction sequence:
1. Seeded demo table with 7 rows across staged backups `f1` and `incr2`.
2. Restored `f1` with PITR target timestamp (`19:52:20`) -> Succeeded, 6 rows, `Healthy Nodes = 3`.
3. Restored `incr2` with no target timestamp -> Succeeded, 7 rows, `Healthy Nodes = 3`.
4. Re-restored `f1` with PITR target timestamp (`19:52:20`) -> Succeeded, 6 rows, `Healthy Nodes = 3`.
5. Navigated to `http://localhost:8080/tables` -> Loaded live table data immediately without hanging.

---

## Lessons Learned & Rules for Distributed Operations

1. **Always Pin Recovery Timelines (`recovery_target_timeline = 'current'`) in Shared Archives**:
   When nodes or repeated restores share an S3/MinIO bucket, `.history` files from earlier forks persist in the archive. Standbys and targeted recovery processes must explicitly configure `recovery_target_timeline = 'current'`. Otherwise, PostgreSQL defaults to `'latest'`, attempts to follow orphaned timeline branches, and aborts with `FATAL: requested timeline X is not a child of this server's history`.
2. **`recovery_target_time` is a Strict Archive Boundary**:
   PostgreSQL archive recovery treats `recovery_target_time` as a mandatory goal. If the archived WAL stream ends prior to the requested timestamp, PostgreSQL assumes an incomplete archive and aborts with `FATAL: recovery ended before configured recovery target was reached`. When specifying target times, the timestamp must not exceed the latest transaction committed in the archived WAL stream.
3. **Never Default to Loopback (`127.0.0.1`) for Remote Node Sidecars**:
   A proxy communicating with sidecars across Docker or Kubernetes networks must never default an unassigned node IP to `127.0.0.1`. When the leader is down, loopback routing hits the proxy itself. Furthermore, control API HTTP clients must use `redirect(Policy::none())` so HTTP 303 redirects are never misconstrued as HTTP 200 success.
4. **Fast-Fail Administrative and UI Pool Queries During Outages**:
   Connection pools configured with failover retry loops (e.g., 30 seconds) are designed for transient database restarts, but cause UI deadlocks when an entire cluster is down. Internal web UI executors must use aggressive failover timeouts (e.g., 3 seconds) so administrative dashboards remain fully responsive even when `Healthy Nodes = 0`.
