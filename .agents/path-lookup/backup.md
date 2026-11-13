### If you want to modify PostgreSQL WAL archiving, basebackup snapshots, or OpenDAL storage, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupManager` managing OpenDAL uploads, WAL archiving/restoration, basebackup snapshotting, `BackupScheduleConfig` (retention and CRON config), retention pruning, and snapshot retrieval/deletion
- `crates/pgvisor-core/src/backup/mod.rs` = Backup module exports, `BasebackupMeta`, `BackupType`, `BackupScheduleConfig`, and `BackupError`
- `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = Leader backup & restore orchestration via `pg_basebackup`, sidecar `/control/restore`, and standby `/control/resync`
- `crates/pgvisor-sidecar/src/api/backup/leader_restore_executor.rs` = Executing local `/control/restore` when leader restores itself
- `crates/pgvisor-sidecar/src/config.rs` = `PostgresConfig` and `ConfigGenerator` generating `restore_command`, `archive_command`, `track_commit_timestamp = on`, and `recovery.signal` for PITR
- `crates/pgvisor-sidecar/src/wal.rs` = `pgvisor-sidecar archive` and `restore` CLI handlers invoking `BackupManager` for PostgreSQL WAL archiving and recovery
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` and `resync_from_primary` for in-place cluster data directory restoration, standby re-cloning, and PITR recovery signaling
- `knowledges/post-mortem-flaky-pitr-restore.md` = Post-mortem detailing cluster restore timeline, split-brain failover race conditions under parallel CPU load, and resolution
- `knowledges/post-mortem-pitr-restore-cluster-failure.md` = Post-mortem detailing missing restore_command fatal crash, silent error swallowing, and standby PGDATA wipe cascade during PITR restore
- `knowledges/pitr-snapshot-selection-and-forward-recovery.md` = Guide and case study explaining forward-only WAL replay mechanics, why snapshots cannot roll backward, and snapshot selection rules for PITR
- `knowledges/post-mortem-timeline-divergence-and-recovery-overrun.md` = Post-mortem detailing multi-timeline divergence on standby recovery, recovery target overrun FATAL error, proxy redirect-follow / self-restore bug, and dead pool timeout
- `docker-compose.yml` = Local S3 credentials configuration for local development

### If you want to modify cluster restore synchronization, standby failover suppression during restore, or proxy pool draining, then check:

- `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = Preparing standbys via `/control/prepare-restore`, restoring leader, and re-syncing standbys with retries
- `crates/pgvisor-sidecar/src/control/handlers.rs` = `handle_prepare_restore`, `handle_restore`, and `handle_resync` setting `ProcessStatus::Restoring` and bumping `restore_epoch`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` preserving `ProcessStatus::Restoring` across `start()`/`stop()`, and setting `Running` only after `wait_ready()` succeeds
- `crates/pgvisor-proxy/src/restore_epoch_tracker.rs` = `RestoreEpochTracker` detecting post-restore `restore_epoch` increment to drain `ConnectionPool`
- `tests/test-timeline-divergence.sh` = Automated regression test verifying timeline branching, standby re-sync on divergent timelines, and repeated PITR restores
- `dev-inspect-timeline.sh` = Inspects cluster timeline IDs, checkpoint locations, roles, divergence, and history across all nodes

### If you want to modify backup metadata (labels, notes, WAL ranges, timeline), snapshot triggering, archive filtering, or backup table views, then check:

- `crates/pgvisor-core/src/backup/archive.rs` = `generate_snapshot_id`, `parse_backup_label`, `process_basebackup_archive` excluding `backup_label.old` and injecting `metadata.json`, `inspect_and_prepare_import_archive` validating and preparing uploaded basebackup archives, and `create_simulated_basebackup`
- `crates/pgvisor-core/src/backup/mod.rs` = re-exports for archive processing, manager, and import validation
- `crates/pgvisor-core/src/backup/manager.rs` = `BasebackupMeta` metadata struct (`snapshot_id`, `backup_id`, `label`, `backup_start_date`, `backup_finish_date`, `timeline`, `start_lsn`, `checkpoint_location`, `backup_from`, `pg_version`, `last_transaction_time`, `last_transaction_xid`), `BackupError::ArchiveFormat`, and OpenDAL storage manager
- `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = `create_backup` formatting snapshot ID with label, capturing leader last committed transaction timestamp/xid, running `pg_basebackup`, and `import_backup` saving uploaded archives to OpenDAL under cluster backup lock
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` cleaning up `backup_label.old` defense-in-depth
- `crates/pgvisor-core/src/api/` = `create_backup_request.rs` and `backup_item_view.rs` (including `last_transaction_time` and `last_transaction_xid`)
- `crates/pgvisor-sidecar/src/api/backup/handlers.rs` = Backup creation, upload/import (`api_upload_backup`), and list endpoints (`/api/backups`)
- `crates/pgvisor-sidecar/src/api/router.rs` = Route registrations for `/api/backups` and `/api/backups/upload` with configurable `max_upload_bytes` body limit
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI table displaying backup items, backup creation modal, and archive import modal (`#uploadModal`) with file upload progress
- `tests/test-backup-restore.sh` = Automated test asserting snapshot ID label prefix, `metadata.json` archive inclusion, `backup_label.old` exclusion, and upload/import endpoint (`/api/backups/upload`) verification
- `scripts/lib/s3-common.sh` = S3 client utility functions (list, get, put, delete, wipe) using curl with AWS SigV4
- `scripts/lib/parse-s3-xml.py` = S3 XML response parser extracting keys, truncation status, and continuation tokens
- `dev-take-backups-snapshot-s3.sh` = Exports entire S3 backups bucket to local snapshot directory for offline bug reproduction
- `dev-import-backups-snapshot-s3.sh` = Imports and restores S3 backups bucket from local snapshot directory
- `dev-inspect-backup.sh` = Inspects backup metadata (snapshot ID, label, created at, last transaction time, last transaction xid) from S3 or local directory
- `tests/test-dev-s3-snapshot.sh` = Automated test verifying S3 snapshot export and import round-trip reproducibility

### If you want to modify Point-In-Time Recovery (PITR), Quick Restore, timeline slider, or snapshot selection, then check:

- `crates/pgvisor-core/src/backup/recovery_target.rs` = `RecoveryTarget` enum (`Time`, `Xid`, `Latest`) for flexible PITR target specification
- `crates/pgvisor-core/src/backup/validation.rs` = Pure recovery target validation (`parse_target_timestamp`, `validate_recovery_target_time`, `validate_recovery_target_xid`, `validate_recovery_target`) verifying format, future timestamps, and lower bounds against snapshot finish dates and baseline XIDs
- `crates/pgvisor-sidecar/src/api/backup/helpers.rs` = `find_best_backup_snapshot` and PITR snapshot/window matching helpers
- `crates/pgvisor-sidecar/src/api/backup/handlers.rs` = `api_quick_restore`, `api_restore_backup`, and `api_pitr_window` returning slider timestamp bounds
- `crates/pgvisor-sidecar/src/config.rs` = `to_postgres_timestamp` normalizing RFC 3339 timestamps (converting `T`/`Z` to `YYYY-MM-DD HH:MM:SS[.f]+00`) before writing `recovery_target_time` into `postgresql.conf`
- `crates/pgvisor-core/src/api/` = `quick_restore_request.rs`, `quick_restore_response.rs`, `pitr_window_response.rs`, `best_backup_query.rs`, `best_backup_response.rs`
- `crates/pgvisor-sidecar/src/api/router.rs` = Route registrations for `/api/backups/quick-restore`, `/api/backups/best`, and `/api/backups/:snapshot_id/pitr-window`
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI quick restore panel, interactive PITR timeline slider, real-time snapshot auto-match hint, restore confirmation modal, and client-side future time validation
- `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = Coordinating sidecar restore, future target validation, standby cancellation via `/control/cancel-restore` on failure, and standby replica re-sync
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` cleaning up `recovery.signal` and resetting configs on failed targeted restore, and `is_running`
- `crates/pgvisor-sidecar/src/control/handlers.rs` = `handle_restore` validating target timestamps and `handle_cancel_restore` (`POST /control/cancel-restore`) returning standbys to running state
- `tests/test-incremental-pitr.sh` = Integration test verifying incremental PITR, forward recovery via `/api/backups/quick-restore` with RFC 3339 `T...Z` timestamps, and rejection of invalid PITR timestamps (future, too-old, malformed) without cluster failure
- `dev-dump-table.sh` = Developer table dump script querying user tables and storing state to `./debug/*`
- `dev-mock-data.sh` = Demo dataset initialization script invoking `dev-dump-table.sh`
- `knowledges/pitr-snapshot-selection-and-forward-recovery.md` = Forward recovery mechanics and snapshot selection rules

### If you want to modify restoring a snapshot on an auto-promoted leader, clearing standby primary_conninfo, or preventing recovery log spam, then check:

- `crates/pgvisor-sidecar/src/supervisor.rs` = `restore_from_snapshot` clearing `primary_conninfo`, deleting residual `standby.signal` and `postgresql.auto.conf`, and reaping `active_child` in `stop()` / `fence()`
- `crates/pgvisor-sidecar/src/control/handlers.rs` = Quorum auto-promotion clearing `cfg.primary_conninfo = None` and `handle_restore` setting role to leader and clearing `primary_conninfo`
- `crates/pgvisor-sidecar/src/api/pg_activity.rs` = Replication lag monitor using `CASE WHEN NOT pg_is_in_recovery()` to prevent WAL control errors during recovery
- `crates/pgvisor-core/src/protocol/tracker.rs` = Query classifier routing `SELECT pg_switch_wal()` as `QueryKind::Write` to leader
- `tests/test-promoted-restore.sh` = Automated regression test verifying leader failover, snapshot restore on promoted leader, absence of recovery spam, and standby re-sync

### If you want to modify backup retention policies, CRON scheduling, cluster-wide backup/restore locks, or follower backup offloading, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupScheduleConfig` (`keep_count`, `retention_days`, `full_backup_cron`, `incremental_backup_cron`, `cron_enabled`, `from_env`), `BasebackupMeta.source_node`, and `prune_retention` with single latest snapshot safeguard
- `crates/pgvisor-sidecar/src/scheduler.rs` = `BackupScheduler` running on the sidecar (active only on Raft leader) driving automated full and incremental backups via `croner::Cron`
- `crates/pgvisor-sidecar/src/control/state.rs` = `SidecarState::backup_lock` holding `BackupLockInfo` with token and TTL expiration timestamps
- `crates/pgvisor-sidecar/src/control/handlers.rs` = `handle_acquire_backup_lock` and `handle_release_backup_lock` enforcing leader-only access (503), TTL auto-expiry, and 409 Conflict rejection
- `crates/pgvisor-sidecar/src/control/server.rs` = Route registration for `/control/backup-lock/acquire` and `/control/backup-lock/release` with cluster auth middleware
- `crates/pgvisor-sidecar/src/api/backup/orchestrator.rs` = Coordinating cluster-wide backup lock on leader sidecar for all backup/restore/delete operations
- `crates/pgvisor-sidecar/src/api/backup/handlers.rs` = `api_create_backup`, `api_restore_backup`, and `api_quick_restore` mapping concurrent lock rejections to HTTP 409 Conflict
- `crates/pgvisor-core/src/api/` = `backup_overview_summary.rs` (`keep_count`) and `backup_item_view.rs` (`source_node`)
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI displaying keep count in summary and source node badge in backup snapshot list
- `crates/pgvisor-sidecar/src/main.rs` = Wiring `BackupScheduleConfig::from_env()` and starting `BackupScheduler`
- `docker-compose.yml` = Canonical compose definition configuring `PGVISOR_BACKUP_*` retention and CRON variables on sidecar nodes
- `examples/docker-compose.yml` = Example compose definition passing `PGVISOR_BACKUP_*` retention and CRON variables with defaults
- `examples/.env.example` & `examples/.env` = Environment variable templates defining default retention limits and CRON schedules
- `tests/test-backup-restore.sh` = Automated integration test asserting follower node backup execution, primary node restore execution, and HTTP 409 Conflict mutex rejection
