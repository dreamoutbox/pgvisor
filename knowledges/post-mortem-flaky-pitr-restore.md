# Post-Mortem: Flaky Incremental PITR Restore Under Concurrent Load

## Incident Summary

| Metric | Value |
|--------|-------|
| **Symptom** | `tests/test-incremental-pitr.sh` failed during parallel execution (`./test.sh -j 5`) at Step [7/8] with `expected 2 rows, got '1'` |
| **Duration** | Failed test ran for 146s (full retry budget exhausted) before failing |
| **Reproduction** | Passed 100% reliably in isolation (~35s), failed intermittently under concurrent test load |
| **Impacted Subsystems** | `pgvisor-sidecar` (heartbeat & failover monitor), `pgvisor-proxy` (backup restore coordination), `tests/test-incremental-pitr.sh` |
| **Root Causes** | (1) Rogue standby failover during leader restore due to missing `Restoring` state, (2) overly tight 400ms HTTP heartbeat timeouts under CPU throttling, (3) proxy standby discovery dropping offline standbys during re-sync, (4) un-bracketed WAL switch routed to read replica |
| **Resolution** | Implemented `ProcessStatus::Restoring`, raised election timeouts to 2500ms / 800ms HTTP timeout, guaranteed full cluster standby re-sync with retries, and added explicit test synchronization |

---

## Detailed Sequence of Failure

```
[Leader: Node 1]                     [Standby: Node 2]                  [Proxy]
       │                                     │                             │
Step 6 │ ◄── RESTORE T0 (1 row) ─────────────┼─────────────────────────────┤
       │     Postgres restored with 1 row    │                             │
       │     Node 2 re-synced (has 1 row) ───┤                             │
       │                                     │                             │
Step 7 │ ◄── RESTORE T2 (2 rows) ────────────┼─────────────────────────────┤
       │ 1. Stops Postgres                   │                             │
       │    (status = "stopped")             │                             │
       │ 2. Wiping PGDATA & untarring snap   │                             │
       │    (takes 2.5s under 5-job load)    │                             │
       │                                     │ 3. Heartbeat checks Node 1  │
       │                                     │    Node 1 status is stopped!│
       │                                     │    Miss 1, Miss 2, Miss 3   │
       │                                     │    (1500ms elapsed)         │
       │                                     │ 4. "No active leader!"      │
       │                                     │    Node 2 PROMOTES ITSELF!  │
       │                                     │    Node 2 role = "leader"   │
       │ 5. Restore completes, starts PG.    │    (Node 2 still has T0!)   │
       │ 6. Split-brain check runs:          │                             │
       │    Node 2 is already leader!        │                             │
       │    Node 1 FENCES ITSELF! ──► [DEAD] │                             │
       │                                     │                             │
       │                                     │ 7. Discovery finds Node 2   │
       │                                     │    as the new leader.       │
       │                                     │                             │
Test:  │                                     │ 8. SELECT COUNT(*) FROM t_pitr
       │                                     │    Routes to Node 2 or 3.   │
       │                                     │    Returns '1' (from T0!)   │
       │                                     │    Expected 2 -> FAIL (146s)│
```

---

## Root Causes

### 1. Standby Heartbeat Misclassified Restoring Leader as Dead
In `crates/pgvisor-sidecar/src/main.rs`, the background heartbeat monitor required `st.status == "running"` to recognize an active leader:
```rust
if st.role == "leader" && st.status == "running" {
    leader_found = true;
}
```
During a cluster restore (`POST /control/restore`), the leader sidecar stopped PostgreSQL to replace PGDATA. Because its status became `"stopped"`, standby peers deemed the leader dead. Under parallel test execution with Docker CPU limits (1 CPU per container, 25 containers active), snapshot extraction and Postgres startup took over 1.5 seconds. Standby nodes hit the 3-miss threshold (1500ms) and initiated a quorum election, promoting standby Node 2.

When Node 1 finished restoring the snapshot with the expected 2 rows, its split-brain guard observed that Node 2 was already operating as leader and immediately fenced Node 1. Node 2 remained the cluster leader, but only held the stale Snapshot T0 state (1 row).

### 2. Overly Tight HTTP Heartbeat Timeouts (400ms)
Both the sidecar heartbeat loop and the proxy discovery loop used a 400ms HTTP client timeout:
```rust
let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_millis(400))
    .build()
```
Under Linux Completely Fair Scheduler (CFS) bandwidth throttling (`cpus: 1`), container threads frequently experienced scheduling latency between 100ms and 300ms. An HTTP request across containers could easily exceed 400ms, causing false heartbeat misses and false node dropouts.

### 3. Proxy Standby List Dropped Recovering Nodes During Restore
In `crates/pgvisor-proxy/src/backup.rs`, `restore_backup` gathered standby addresses solely from `self.standby_addrs`:
```rust
let standbys = self.standby_addrs.read().await.clone();
```
`self.standby_addrs` was dynamically updated by the proxy background discovery loop, which excluded any node not in `"running"` status. If a standby was restarting or re-cloning from an earlier step, it was omitted from `standby_addrs`, meaning `restore_backup` never dispatched `/control/resync` to it. Furthermore, any failed `/control/resync` calls were logged as warnings without retries.

### 4. Read/Write Splitting Routed WAL Switch to Standby
In `tests/test-incremental-pitr.sh`, Step 4 executed:
```bash
run_sql "SELECT pg_switch_wal();" > /dev/null || true
```
Because the query began with `SELECT` and was not inside a transaction block, `pgvisor-proxy` classified it as `QueryKind::Read` and routed it to a read replica. Standby replicas rejected `pg_switch_wal()` with `ERROR: recovery is in progress`. The `|| true` hid this error, preventing the WAL switch from actually executing on the leader.

---

## Changes Implemented

### 1. Explicit `ProcessStatus::Restoring` State
- **File**: `crates/pgvisor-sidecar/src/supervisor.rs`
  - Added `ProcessStatus::Restoring` to the `ProcessStatus` enum.
  - In `restore_from_snapshot` and `resync_from_primary`, set `*st = ProcessStatus::Restoring` immediately after stopping Postgres so the node continuously reports that it is actively restoring.

- **File**: `crates/pgvisor-sidecar/src/main.rs`
  - Updated `handle_status` to report `"restoring"`.
  - In the heartbeat monitor:
    - If `local_status == ProcessStatus::Restoring`, paused auto-failover actions and cleared missed heartbeats.
    - If `local_role == "leader"`, skipped the split-brain fence check while restoring.
    - In standby probe loop, treated peer leaders with `st.status == "restoring"` as alive and valid (`leader_found = true`), preventing rogue standby elections during administrative restores.

### 2. Hardened Heartbeat & Election Budgets
- **Files**: `crates/pgvisor-sidecar/src/main.rs` & `crates/pgvisor-proxy/src/main.rs`
  - Increased HTTP client timeout from `400ms` to `800ms`.
  - Increased standby failover election threshold from 3 misses (1500ms) to 5 misses (2500ms), giving enough margin for container scheduling jitter while remaining within the 1500ms..3000ms specification in Ticket 004.

### 3. Reliable Standby Re-Sync with Retries
- **Files**: `crates/pgvisor-proxy/src/backup.rs` & `crates/pgvisor-proxy/src/main.rs`
  - Passed configured standby targets (`configured_standbys`) to `ProxyBackupService`.
  - In `restore_backup`, computed the union of dynamic standbys and configured standbys (excluding the leader) to guarantee that every standby receives `/control/resync`.
  - Added a 3-attempt retry loop with 1-second backoff for `/control/resync` calls to absorb transient node initialization delays.
  - In proxy discovery, mapped `"restoring"` node status to `NodeHealthState::Degraded` and preserved `discovered_leader` so the proxy does not lose track of the leader during restore.

### 4. Test Script Synchronization & WAL Switch Fix
- **File**: `tests/test-incremental-pitr.sh`
  - Routed `pg_switch_wal()` to the leader inside a transaction block (`BEGIN; SELECT pg_switch_wal(); COMMIT;`) or via direct container execution.
  - Added `wait_for_healthy 60` and `wait_for_proxy_ready` after each restore step to guarantee all cluster containers and the proxy are healthy before verifying row counts.

---

## Verification & Results

1. **Unit & Integration Compilation**:
   ```bash
   cargo check --all-targets
   # Clean exit (code 0)
   ```
2. **Full Parallel Test Suite (`./test.sh -j 5`)**:
   - 15 concurrent test suites (25 containers total).
   - `test-incremental-pitr.sh` passed in 50s (reduced from 146s failure timeout).
   - All 15 tests passed with 0 failures:
     ```text
     =============================================================
       Test Results Summary
     =============================================================
       test-cluster-crud.sh       : PASSED (27s)
       test-backup-restore.sh     : PASSED (40s)
       test-incremental-pitr.sh   : PASSED (50s)
       test-failover.sh           : PASSED (45s)
       test-auto-rejoin.sh        : PASSED (49s)
       test-rejoin-fenced.sh      : PASSED (42s)
       test-add-node.sh           : PASSED (39s)
       test-switchover.sh         : PASSED (37s)
       test-users-permissions.sh  : PASSED (21s)
       test-transaction.sh        : PASSED (27s)
       test-routing.sh            : PASSED (44s)
       test-audit-logs.sh         : PASSED (41s)
       test-double-failure.sh     : PASSED (56s)
       test-proxy-failover.sh     : PASSED (31s)
       test-metrics.sh            : PASSED (34s)
     =============================================================
       Total: 15 | Passed: 15 | Failed: 0 | Duration: 133s
     =============================================================
     ```

---

## Lessons Learned & Rules for Distributed Operations

1. **Administrative Maintenance Must Be Observable to Consensus**:
   When a node intentionally stops its database for snapshot restoration or data re-cloning, it must never appear as a crash to peer nodes. Exposing explicit lifecycle states (`Restoring`) in control APIs prevents rogue elections and split-brain fencing.
2. **Never Rely Solely on Ephemeral Dynamic Health Pools for Re-Sync**:
   Administrative workflows like cluster restore must re-sync all cluster members known to configuration, not just those that happened to be in the healthy query routing pool at that exact millisecond.
3. **Respect Read/Write Splitting In Test Fixtures**:
   Because `pgvisor-proxy` splits on the SQL verb, operational commands like `SELECT pg_switch_wal()` must be wrapped in `BEGIN ... COMMIT` blocks to force leader routing.
