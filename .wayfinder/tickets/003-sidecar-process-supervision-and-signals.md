---
id: "003"
title: "Sidecar Process Supervision and Signals"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How does the sidecar manage signal forwarding (SIGTERM, SIGINT, SIGQUIT), child process reaping, zombie reaping as container PID 1, pipe logging of Postgres stdout/stderr, and config generation (`postgresql.conf`, `pg_hba.conf`) on startup?

## Resolution

Implemented PostgreSQL container PID 1 process supervision and automated configuration in `crates/pgvisor-sidecar`:
1. **Automated Configuration Generator (`pgvisor-sidecar/src/config.rs`)**:
   - Generates customized `postgresql.conf` (port, wal_level=replica, wal_senders, wal_keep_size, hot_standby, archive_command).
   - Manages `standby.signal` and `primary_conninfo` for replication standbys.
   - Generates secure default `pg_hba.conf` supporting local socket trust and network client authentication.
2. **PostgreSQL Process Supervision (`pgvisor-sidecar/src/supervisor.rs`)**:
   - `ensure_initdb`: Initializes database cluster if absent.
   - Child process spawning: Spawns `postgres -D <data_dir>` with piped stdout/stderr, asynchronously streaming lines into structured `tracing::info!(target: "postgres", ...)` logs.
   - `promote`: Executes `pg_ctl promote -D <data_dir>`.
   - `fence`: Triggers immediate hard stop (`pg_ctl stop -m immediate` or SIGQUIT to child) upon consensus quorum loss, preventing split-brain writes.
3. **Container PID 1 Signal Trapping (`pgvisor-sidecar/src/main.rs`)**:
   - Traps Unix signals (`SIGTERM`, `SIGINT`, `SIGQUIT`).
   - Maps `SIGTERM` / `SIGINT` to graceful fast checkpoint shutdown (`pg_ctl stop -m fast`).
   - Maps `SIGQUIT` to immediate fencing.
4. **Unit Tests**:
   - Verified primary configuration generation with archive command.
   - Verified standby configuration generation with `standby.signal` and `primary_conninfo`.
   - Verified supervisor initial state tracking and child PID reporting.
