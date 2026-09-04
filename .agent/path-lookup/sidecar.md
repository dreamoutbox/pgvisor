### If you want to modify PostgreSQL sidecar process supervision, configuration templating, signals, or fencing, then check:

- `crates/pgvisor-sidecar/src/config.rs` = PostgreSQL configuration generator for `postgresql.conf`, `pg_hba.conf`, and replication `standby.signal`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing `initdb`, replica cloning via `pg_basebackup`, child process spawning, pipe logging, `pg_ctl promote` (idempotent), `repoint_primary`, emergency fencing (`pg_ctl stop -m immediate`), `restore_from_snapshot`, and `resync_from_primary`
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar service entry point, container PID 1 signal listener (`SIGTERM`, `SIGINT`, `SIGQUIT`), Axum HTTP control server (`/control/status`, `/control/restore`, `/control/resync`, `/control/promote`, `/control/fence`, `/control/repoint`), auto-failover election monitor, and split-brain active fencing
