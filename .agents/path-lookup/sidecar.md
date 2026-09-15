### If you want to modify PostgreSQL sidecar process supervision, configuration templating, signals, or fencing, then check:

- `crates/pgvisor-sidecar/src/config.rs` = PostgreSQL configuration generator for `postgresql.conf`, `pg_hba.conf`, replication `standby.signal`, and PITR `recovery.signal` / `restore_command`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing `initdb`, replica cloning via `pg_basebackup`, child process spawning, pipe logging, `start`, `stop`, `restart`, `pg_ctl promote` (idempotent), `repoint_primary`, emergency fencing (`pg_ctl stop -m immediate`), `restore_from_snapshot`, and `resync_from_primary`
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar service entry point, WAL `archive` / `restore` CLI subcommands, container PID 1 signal listener (`SIGTERM`, `SIGINT`, `SIGQUIT`), Axum HTTP control server (`/control/status`, `/control/start`, `/control/stop`, `/control/restart`, `/control/restore`, `/control/resync`, `/control/promote`, `/control/fence`, `/control/repoint`), auto-failover election monitor, and split-brain active fencing

### If you want to test adding a new node dynamically to the cluster or scale out standby replicas, then check:

- `tests/test-add-node.sh` = Integration test verifying dynamic container launch, sidecar status, `pg_basebackup` historical clone, leader WAL sender scaling, and streaming replication
- `composes/docker-compose.add-node4.yml` = `pgvisor-node4` service definition and `node4_data` volume override
- `crates/pgvisor-sidecar/src/supervisor.rs` = `ensure_initialized` standby clone logic via `pg_basebackup`

### If you want to modify stopped leader restart, standby repoint, or split-brain prevention on startup, then check:

- `crates/pgvisor-sidecar/src/main.rs` = `start_postgres_safely` pre-start peer leader discovery, `handle_repoint` accepting repoint while stopped/fenced (guards leader-down highlight to running nodes only), and `SidecarState.peers`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::repoint_primary` (skips `pg_ctl reload` if Postgres is not running), `wait_ready`, preserving `ProcessStatus::Fenced`, and fallback to `resync_from_primary`
- `tests/test-restart-leader.sh` = Integration test verifying leader stop, failover to standby, leader restart without split-brain, and streaming replication catch-up

### If you want to modify manual leader switchover, demoted leader fencing, or auto-rejoin as standby, then check:

- `crates/pgvisor-sidecar/src/main.rs` = `handle_demote` setting `ProcessStatus::Fenced` and background election monitor loop allowing fenced nodes to auto-rejoin under active leader
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::set_status` and `resync_from_primary` resetting status on error
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService::switchover` orchestrating demotion of current leader, promotion of target standby, and repointing of remaining standbys
- `tests/test-switchover.sh` = Integration test verifying graceful switchover, demoted leader auto-rejoin, streaming replication, and second reverse switchover
