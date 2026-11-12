# Implementation Plan: Staged Database Restore & Rollback Protection

## Goal
Implement a non-destructive, atomic shadow-recovery restore process on the cluster leader so that if a Point-In-Time-Recovery (PITR) or snapshot restore fails (e.g. invalid target time, missing WAL, corrupt archive), the current PostgreSQL instance remains running on port 5432, node 1 retains the leader role, follower replicas do not replicate corrupted/empty data, and client connections are buffered rather than dropped.

---

### Phase 1: Proxy Client Connection Pausing and Buffering [STATUS: DONE]
- Goal: Enable `pgvisor-proxy` to temporarily pause and buffer client connections during cluster restore, unpausing them immediately if restore fails or resuming after timeline switch on success.
- Tasks:
  - [x] Add atomic `paused` state and condition variable / notify mechanism to `ConnectionPool` (`crates/pgvisor-proxy/src/pool.rs`).
  - [x] Implement `pause_client_traffic()` and `resume_client_traffic()` on `ConnectionPool`.
  - [x] Update client query dispatch in `crates/pgvisor-proxy/src/server.rs` to await resume if paused (with a configurable timeout, e.g. 60s, returning `57P03 - cannot_connect_now` if timeout expires).
  - [x] Wrap `ProxyBackupService::restore_cluster` (`crates/pgvisor-proxy/src/backup.rs`) to pause client queries before notifying nodes, resuming immediately on failure or draining and resuming on success.
- Done when: `cargo nextest run -p pgvisor-proxy` passes, and proxy integration test confirms client queries pause during restore and resume cleanly.

---

### Phase 2: Shadow Staging Directory & Process Spawning in Sidecar [STATUS: DONE]
- Goal: Add shadow PostgreSQL process management to `PostgresSupervisor` to perform recovery in a staging directory alongside `pgdata`.
- Tasks:
  - [x] Define staging path helper `staging_data_dir(&self) -> PathBuf` (e.g. `/var/lib/postgresql/data/pgdata_staging` on the same volume).
  - [x] Implement staging directory cleanup and archive extraction into `pgdata_staging` without touching the active `pgdata`.
  - [x] Implement `spawn_shadow_postgres()` in `crates/pgvisor-sidecar/src/supervisor.rs` running on staging port `config.port + 1` (5433) using `/tmp` unix socket directory.
  - [x] Generate staging `postgresql.conf` with `port = 5433` and PITR `recovery.signal` / `restore_command`.
- Done when: Unit tests in `crates/pgvisor-sidecar/src/supervisor.rs` verify staging path generation and staging configuration generation.

---

### Phase 3: Shadow Recovery Validation, Promotion, and Rollback on Failure [STATUS: DONE]
- Goal: Execute PITR recovery in the shadow process and guarantee automatic rollback and cleanup if recovery fails.
- Tasks:
  - [x] Implement `wait_shadow_ready(port: 5433, timeout_secs)` to wait for shadow PostgreSQL recovery and promote.
  - [x] In `crates/pgvisor-sidecar/src/supervisor.rs`, implement shadow recovery execution:
    - If shadow process fails or exits with non-zero status (e.g. target time not reached):
      - Kill shadow process if still running.
      - Remove `pgdata_staging` completely.
      - Keep active PostgreSQL process (5432) running.
      - Keep supervisor status as `ProcessStatus::Running` and role as `leader`.
      - Return descriptive `SupervisorError::CommandFailed` error to caller.
- Done when: Simulated failed restore in tests cleans up `pgdata_staging`, leaves active Postgres running, and returns error without altering active process state.

---

### Phase 4: Atomic Directory Swap and Resync on Recovery Success [STATUS: DONE]
- Goal: Atomically swap verified staging data into production location and trigger follower replica realignment only after successful leader promote.
- Tasks:
  - [x] Upon shadow recovery success, cleanly stop shadow process (`pg_ctl stop -m fast`).
  - [x] Stop active PostgreSQL process on 5432.
  - [x] Atomically swap directories:
    - `rename(pgdata, pgdata_old)`
    - `rename(pgdata_staging, pgdata)`
    - `remove_dir_all(pgdata_old)`
  - [x] Re-template `postgresql.conf` for port 5432 with clean settings (removing `recovery.signal` and `recovery_target_*`).
  - [x] Start new primary PostgreSQL process on port 5432.
  - [x] Update `handle_restore` in `crates/pgvisor-sidecar/src/control/handlers.rs` to return success only after port 5432 is healthy.
  - [x] Ensure standbys receive `/control/resync` only when leader restore returns 200 OK.
- Done when: Successful restore switches database to restored timeline, starts on 5432, and followers resync via `pg_basebackup`.

---

### Phase 5: Election Monitor Leadership Retention & End-to-End Test [STATUS: DONE]
- Goal: Prevent standbys from attempting elections when restore fails, and provide regression test reproducing the invalid target time scenario.
- Tasks:
  - [x] Verify `crates/pgvisor-sidecar/src/election.rs` treats leader as active while restoring, and leader immediately returns to `Running` (never `Stopped`) if restore fails.
  - [x] Create test script `tests/test-restore-failure-rollback.sh`:
    - Seed test data on cluster.
    - Attempt restore with invalid future/uncovered target time.
    - Assert restore returns HTTP error.
    - Assert leader node 1 remains online and healthy.
    - Assert node 2 does NOT take over leadership.
    - Assert original database data on node 1 is preserved and accessible.
- Done when: `./tests/test-restore-failure-rollback.sh` passes end-to-end against live cluster.

---

## Unknowns, risks, assumptions
- **Volume Disk Space**: Because `pgdata_staging` lives on the same volume as `pgdata`, the volume must have sufficient free space to hold two copies of the base database during the brief recovery window.
- **Port 5433 Availability**: Port 5433 inside the container is private to localhost and unix sockets; ensure no other container process binds 5433.

