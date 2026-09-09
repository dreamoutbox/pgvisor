### If you want to modify PostgreSQL sidecar process supervision, configuration templating, signals, or fencing, then check:

- `crates/pgvisor-sidecar/src/config.rs` = PostgreSQL configuration generator for `postgresql.conf`, `pg_hba.conf`, and replication `standby.signal`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing `initdb`, replica cloning via `pg_basebackup`, child process spawning, pipe logging, `pg_ctl promote` (idempotent), `repoint_primary`, emergency fencing (`pg_ctl stop -m immediate`), `restore_from_snapshot`, and `resync_from_primary`
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar service entry point, container PID 1 signal listener (`SIGTERM`, `SIGINT`, `SIGQUIT`), Axum HTTP control server (`/control/status`, `/control/restore`, `/control/resync`, `/control/promote`, `/control/fence`, `/control/repoint`), auto-failover election monitor, and split-brain active fencing

### If you want to test adding a new node dynamically to the cluster or scale out standby replicas, then check:

- `tests/test-add-node.sh` = Integration test verifying dynamic container launch, sidecar status, `pg_basebackup` historical clone, leader WAL sender scaling, and streaming replication
- `composes/docker-compose.add-node4.yml` = `pgvisor-node4` service definition and `node4_data` volume override
- `crates/pgvisor-sidecar/src/supervisor.rs` = `ensure_initialized` standby clone logic via `pg_basebackup`
