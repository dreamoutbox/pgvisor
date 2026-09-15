# Postmortem: Promoted Leader Restore Recovery Loop and Log Spam

**Date:** 2026-09-15
**Severity:** High
**Status:** Resolved

## Summary
When restoring an incremental snapshot on an auto-promoted leader (`node2`) without specifying a PITR target time, the node booted into standby recovery mode connecting to dead `node1` instead of operating as a read-write primary. The proxy's background monitor queried `pg_current_wal_lsn()` on the recovering node, flooding logs with `ERROR: recovery is in progress` and breaking cluster read/write availability.

## Impact
Cluster write operations failed following a snapshot restore on an auto-promoted leader. The database remained stuck in recovery mode, standby replicas could not establish streaming replication, and logs were inundated with WAL control function errors.

## Timeline
- `07:15` — Cluster initialized with Node 1 as leader and demo data seeded (`f1` full backup, `incr2` incremental backup).
- `07:20` — Node 1 stopped; Node 2 successfully auto-promoted to cluster leader.
- `07:22` — Snapshot `incr2` restored on Node 2 without specifying a PITR target time.
- `07:23` — Node 2 started with residual `standby.signal` and `primary_conninfo` pointing to dead Node 1, remaining stuck in read-only recovery.
- `07:24` — Proxy replication lag query repeatedly executed `pg_current_wal_lsn()` on Node 2, triggering continuous recovery errors.
- `07:55` — Root cause identified in sidecar config retention, supervisor child reaping, and proxy replication queries.
- `08:23` — Fix implemented, verified, and automated regression test `tests/test-promoted-restore.sh` passed.

## Root Cause
1. **Config retention across promotion:** When Node 2 auto-promoted after Node 1 went down, `monitor_state.config.primary_conninfo` was not reset to `None` in memory.
2. **Missing primary reset on restore:** `handle_restore` in `pgvisor-sidecar` and `restore_from_snapshot` in `supervisor.rs` reused the active configuration without clearing `primary_conninfo`. Consequently, `ConfigGenerator::write_configs` generated `standby.signal` and `primary_conninfo = host=pgvisor-node1...`.
3. **Residual snapshot artifacts:** The extracted snapshot archive contained `standby.signal` and `postgresql.auto.conf` from the baseline node, which were not cleaned up prior to process start.
4. **Unreaped child processes:** In `supervisor.rs`, `self.stop()` and `self.fence()` did not reap or clear `self.active_child` on successful `pg_ctl stop`, leaving stale child handles that caused `wait_ready` fast-fail checks to report unexpected exits.
5. **Unguarded WAL query in proxy:** `pgvisor-proxy`'s background monitor executed `pg_current_wal_lsn()` directly without verifying `NOT pg_is_in_recovery()`, generating continuous log spam when the node was in recovery.
6. **Query classification misroute:** `SELECT pg_switch_wal()` was classified as read-only by `QueryTracker`, routing WAL switch commands away from the primary.

## Detection
User manual test sequence:
1. `./reset-docker-compose.sh`
2. `./dev-setup-demo-data.sh`
3. Stop leader Node 1.
4. Node 2 auto-promotes to leader.
5. Restore snapshot `incr2` without specifying PITR target time.
6. Observed error log spam on Node 2:
   ```text
   ERROR:  recovery is in progress
   HINT:  WAL control functions cannot be executed during recovery.
   STATEMENT:  SELECT client_addr::text, application_name, COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn), 0)::text FROM pg_stat_replication;
   ```

## Resolution
1. **Clear primary_conninfo on promotion & restore:** Updated `pgvisor-sidecar/src/main.rs` to clear `cfg.primary_conninfo = None` during auto-promotion and within `handle_restore`.
2. **Purge residual signals and configs:** Updated `PostgresSupervisor::restore_from_snapshot` and `promote` to remove `standby.signal` and `postgresql.auto.conf` before starting PostgreSQL.
3. **Reap supervisor active child:** Updated `PostgresSupervisor::stop` and `fence` to take and wait on `self.active_child` when `pg_ctl stop` succeeds cleanly.
4. **Guard replication lag query:** Wrapped `pg_current_wal_lsn()` with `CASE WHEN NOT pg_is_in_recovery() THEN ... ELSE '0' END` in `pgvisor-proxy/src/main.rs`.
5. **Route WAL switch to leader:** Classified `pg_switch_wal()` as `QueryKind::Write` in `pgvisor-core/src/protocol/tracker.rs`.
6. **Regression test suite:** Added `tests/test-promoted-restore.sh` with dedicated compose profile `composes/docker-compose.promoted-restore.yml` to assert no recovery spam and verify read-write availability post-restore.

## Lessons Learned / Action Items
- [x] In-place cluster restore must unconditionally enforce primary role and clear replication parameters.
- [x] Always clean up `standby.signal` and `postgresql.auto.conf` after extracting snapshot tarballs.
- [x] Process supervisor must reap child process handles upon clean external process termination (`pg_ctl`).
- [x] Monitor queries against primary nodes must defensively check recovery state before invoking WAL control functions.
- [x] Added automated regression test `tests/test-promoted-restore.sh` to CI/test suite.
