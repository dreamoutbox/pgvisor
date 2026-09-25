### If you want to modify PostgreSQL sidecar process supervision, configuration templating, signals, or fencing, then check:

- `crates/pgvisor-sidecar/src/config.rs` = PostgreSQL configuration generator for `postgresql.conf`, `pg_hba.conf`, replication `standby.signal`, and PITR `recovery.signal` / `restore_command`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` managing `initdb`, replica cloning via `pg_basebackup`, child process spawning, pipe logging and circular buffer capture (`append_log`, `recent_logs`), diagnostic/config file reading (`read_node_file`), `start`, `stop`, `restart`, `pg_ctl promote` (idempotent), `repoint_primary`, emergency fencing (`pg_ctl stop -m immediate`), `restore_from_snapshot`, and `resync_from_primary`
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Axum HTTP control request handlers (`/control/status`, `/control/logs`, `/control/config/:config_type`, `/control/start`, `/control/stop`, `/control/restart`, `/control/restore`, `/control/resync`, `/control/promote`, `/control/fence`, `/control/repoint`, `/control/events`) and `start_postgres_safely`
- `crates/pgvisor-sidecar/src/control/server.rs` = Axum router construction and listener spawning for sidecar control API
- `crates/pgvisor-sidecar/src/control/state.rs` = `SidecarState`, `SidecarEventRecord`, `LogsQuery`, and control request/response payload DTOs
- `crates/pgvisor-core/src/node.rs` = `NodeConfigType` enum, `NodeLogEntry`, `NodeLogsResponse`, and `NodeConfigResponse` transfer models
- `crates/pgvisor-sidecar/src/election.rs` = Background heartbeat loop & auto-failover election monitor, quorum promotion, split-brain leader detection/fencing, and standby repoint broadcast
- `crates/pgvisor-sidecar/src/wal.rs` = WAL `archive` / `restore` CLI subcommand handlers
- `crates/pgvisor-sidecar/src/version.rs` = PostgreSQL server version detection
- `crates/pgvisor-sidecar/src/main.rs` = Sidecar service entrypoint, CLI dispatch, environment configuration, component wiring, and container PID 1 signal listener (`SIGTERM`, `SIGINT`, `SIGQUIT`)

### If you want to inspect node PostgreSQL logs, postgresql.conf, pg_hba.conf, or runtime diagnostic files:

- `crates/pgvisor-core/src/node.rs` = `NodeConfigType` enum (`PostgresqlConf`, `PostgresqlAutoConf`, `PgHbaConf`, `PgIdentConf`, `PostmasterPid`, `PostmasterOpts`, `StandbySignal`, `RecoverySignal`, `BackupLabel`), `NodeLogEntry`, and DTO responses
- `crates/pgvisor-sidecar/src/logging.rs` = `SidecarLogCaptureLayer` capturing structured tracing logs from sidecar and PostgreSQL into in-memory ring buffer
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::recent_logs` ring buffer retrieval and `PostgresSupervisor::read_node_file` safe filesystem inspection
- `crates/pgvisor-sidecar/src/control/handlers.rs` = `GET /control/logs` and `GET /control/config/:config_type` handlers
- `crates/pgvisor-sidecar/src/control/server.rs` = Route registration under cluster auth middleware

### If you want to test adding a new node dynamically to the cluster or scale out standby replicas, then check:

- `tests/test-add-node.sh` = Integration test verifying dynamic container launch, sidecar status, `pg_basebackup` historical clone, leader WAL sender scaling, and streaming replication
- `composes/docker-compose.add-node4.yml` = `pgvisor-node4` service definition and `node4_data` volume override
- `crates/pgvisor-sidecar/src/supervisor.rs` = `ensure_initialized` standby clone logic via `pg_basebackup`

### If you want to modify stopped leader restart, standby repoint, or split-brain prevention on startup, then check:

- `crates/pgvisor-sidecar/src/control/handlers.rs` = `start_postgres_safely` pre-start peer leader discovery, `handle_repoint` accepting repoint while stopped/fenced (guards leader-down highlight to running nodes only)
- `crates/pgvisor-sidecar/src/control/state.rs` = `SidecarState.peers` configuration
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::repoint_primary` (skips `pg_ctl reload` if Postgres is not running), `wait_ready`, preserving `ProcessStatus::Fenced`, and fallback to `resync_from_primary`
- `tests/test-restart-leader.sh` = Integration test verifying leader stop, failover to standby, leader restart without split-brain, and streaming replication catch-up

### If you want to modify manual leader switchover, demoted leader fencing, or auto-rejoin as standby, then check:

- `crates/pgvisor-sidecar/src/control/handlers.rs` = `handle_demote` setting `ProcessStatus::Fenced`
- `crates/pgvisor-sidecar/src/election.rs` = Background election monitor loop allowing fenced nodes to auto-rejoin under active leader
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::set_status` and `resync_from_primary` resetting status on error
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService::switchover` orchestrating demotion of current leader, promotion of target standby, and repointing of remaining standbys
- `tests/test-switchover.sh` = Integration test verifying graceful switchover, demoted leader auto-rejoin, streaming replication, and second reverse switchover

### If you want to modify proxy-to-sidecar or sidecar-to-sidecar cluster authentication, then check:

- `crates/pgvisor-core/src/auth.rs` = HMAC-SHA256 bearer token derivation, header formatting, and constant-time validation functions
- `crates/pgvisor-sidecar/src/control/auth.rs` = Axum middleware enforcing `PGVISOR_CLUSTER_SECRET` on control endpoints
- `crates/pgvisor-sidecar/src/control/server.rs` = Axum control router applying cluster auth middleware
- `crates/pgvisor-sidecar/src/control/state.rs` = `SidecarState.cluster_secret` field and constructor
- `crates/pgvisor-sidecar/src/control/handlers.rs` = `start_postgres_safely` peer leader discovery client with auth header
- `crates/pgvisor-sidecar/src/election.rs` = `spawn_election_monitor` peer heartbeat and repoint broadcast client with auth header
- `crates/pgvisor-proxy/src/main.rs` = Proxy reading `PGVISOR_CLUSTER_SECRET` and configuring topology monitor client
- `crates/pgvisor-proxy/src/cluster.rs` = `ProxyClusterService::with_cluster_secret` attaching auth header to switchover/node lifecycle calls
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::with_cluster_secret` attaching auth header to restore/resync calls
- `knowledges/proxy-sidecar-communication.md` = Documentation on HTTP control API, consensus over Axum, and cluster secret authentication
- `docker-compose.yml` = `PGVISOR_CLUSTER_SECRET` environment variable distribution across cluster services

### If you want to modify sidecar TLS/SSL certificate generation or PostgreSQL SSL settings, then check:

- `crates/pgvisor-core/src/tls.rs` = `TlsCertPair` self-signed cert generation via `rcgen`, certificate and key file persistence with 0600 permissions
- `crates/pgvisor-sidecar/src/config.rs` = `PostgresConfig` `ssl`, `ssl_cert_file`, `ssl_key_file` fields, and `write_configs` appending `ssl = on/off` to `postgresql.conf`
- `crates/pgvisor-sidecar/src/main.rs` = Reading `PGVISOR_TLS_ENABLED`, `PGVISOR_TLS_CERT_FILE`, `PGVISOR_TLS_KEY_FILE`, staging external/mounted keys to `$PGDATA/server.key` with 0600 mode for PostgreSQL compatibility, loading/generating node certificates, and configuring replication `sslmode=prefer`
- `docker-compose.yml` = `PGVISOR_TLS_ENABLED`, `PGVISOR_TLS_CERT_FILE`, and `PGVISOR_TLS_KEY_FILE` environment variables on sidecar node services
- `docker-compose.with-tls.yml` = Standalone cluster compose definition with end-to-end TLS enabled and volume mounts for node/proxy certificates
