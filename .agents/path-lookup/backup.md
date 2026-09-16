### If you want to modify PostgreSQL WAL archiving, basebackup snapshots, or OpenDAL storage, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupManager` managing OpenDAL uploads, WAL archiving/restoration, basebackup snapshotting, `BackupScheduleConfig` (MinIO dev target, hourly incremental, midnight full backup), retention pruning, and snapshot retrieval/deletion
- `crates/pgvisor-core/src/backup/mod.rs` = Backup module exports, `BasebackupMeta`, `BackupType`, `BackupScheduleConfig`, and `BackupError`
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService` orchestrating live physical snapshots via `pg_basebackup`, cluster restore coordination via leader sidecar `/control/restore` and standby `/control/resync`, and connection pool draining
- `crates/pgvisor-sidecar/src/config.rs` = `PostgresConfig` and `ConfigGenerator` generating `restore_command`, `archive_command`, and `recovery.signal` for PITR
- `crates/pgvisor-sidecar/src/main.rs` = `pgvisor-sidecar archive` and `restore` CLI handlers invoking `BackupManager` for PostgreSQL WAL archiving and recovery
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` and `resync_from_primary` for in-place cluster data directory restoration, standby re-cloning, and PITR recovery signaling
- `knowledges/post-mortem-flaky-pitr-restore.md` = Post-mortem detailing cluster restore timeline, split-brain failover race conditions under parallel CPU load, and resolution
- `knowledges/post-mortem-pitr-restore-cluster-failure.md` = Post-mortem detailing missing restore_command fatal crash, silent error swallowing, and standby PGDATA wipe cascade during PITR restore
- `knowledges/pitr-snapshot-selection-and-forward-recovery.md` = Guide and case study explaining forward-only WAL replay mechanics, why snapshots cannot roll backward, and snapshot selection rules for PITR
- `knowledges/post-mortem-timeline-divergence-and-recovery-overrun.md` = Post-mortem detailing multi-timeline divergence on standby recovery, recovery target overrun FATAL error, proxy redirect-follow / self-restore bug, and dead pool timeout
- `docker-compose.yml` = Local MinIO and S3 credentials configuration for local development

### If you want to modify cluster restore synchronization, standby failover suppression during restore, or timeline divergence recovery, then check:

- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::restore_backup` notifying standbys via `/control/prepare-restore`, restoring leader, and re-syncing standbys with retries
- `crates/pgvisor-sidecar/src/main.rs` = `handle_prepare_restore`, `handle_restore`, and `handle_resync` setting `ProcessStatus::Restoring`, and heartbeat loop treating responding leader as alive
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor` preserving `ProcessStatus::Restoring` across `start()`/`stop()`, and setting `Running` only after `wait_ready()` succeeds
- `tests/test-timeline-divergence.sh` = Automated regression test verifying timeline branching, standby re-sync on divergent timelines, and repeated PITR restores

### If you want to modify backup metadata (labels, notes, WAL ranges, timeline), snapshot triggering, archive filtering, or backup table views, then check:

- `crates/pgvisor-core/src/backup/archive.rs` = `generate_snapshot_id`, `parse_backup_label`, `process_basebackup_archive` excluding `backup_label.old` and injecting `metadata.json`, and `create_simulated_basebackup`
- `crates/pgvisor-core/src/backup/mod.rs` = re-exports for archive processing and manager
- `crates/pgvisor-core/src/backup/manager.rs` = `BasebackupMeta` metadata struct (`snapshot_id`, `backup_id`, `label`, `backup_start_date`, `backup_finish_date`, `timeline`, `start_lsn`, `checkpoint_location`, `backup_from`, `pg_version`) and OpenDAL storage manager
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::create_backup` formatting snapshot ID with label, running `pg_basebackup`, and invoking `process_basebackup_archive`
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` cleaning up `backup_label.old` defense-in-depth
- `crates/pgvisor-dashboard/src/models.rs` = `CreateBackupRequest` and `BackupItemView` view models
- `crates/pgvisor-dashboard/src/handlers.rs` = Backup creation and list endpoints (`/api/backups`) and page renderer mapping metadata to `BackupItemView`
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI table displaying backup items (Snapshot ID, Type, Optional Label / Note, Created At, Size, WAL range, actions) and creation modal
- `tests/test-backup-restore.sh` = Automated test asserting snapshot ID label prefix, `metadata.json` archive inclusion, and `backup_label.old` exclusion

### If you want to modify Point-In-Time Recovery (PITR), Quick Restore, or snapshot selection, then check:

- `crates/pgvisor-dashboard/src/handlers.rs` = `find_best_backup_snapshot` (resolving closest prior basebackup snapshot to target), `parse_target_timestamp`, `BackupService::quick_restore`, `api_quick_restore`, and `api_find_best_backup`
- `crates/pgvisor-dashboard/src/models.rs` = `QuickRestoreRequest`, `QuickRestoreResponse`, `BestBackupQuery`, and `BestBackupResponse` models
- `crates/pgvisor-dashboard/src/lib.rs` = Route registrations for `/api/backups/quick-restore` and `/api/backups/best`
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI quick restore panel, real-time snapshot auto-match hint, and single-click restore confirmation modal
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::restore_backup` coordinating sidecar restore, standby replica re-sync, and connection pool draining
- `dev-dump-table.sh` = Developer table dump script querying user tables and storing state to `./debug/*`
- `dev-setup-demo-data.sh` = Demo dataset initialization script invoking `dev-dump-table.sh`
- `knowledges/pitr-snapshot-selection-and-forward-recovery.md` = Forward recovery mechanics and snapshot selection rules

### If you want to modify restoring a snapshot on an auto-promoted leader, clearing standby primary_conninfo, or preventing recovery log spam, then check:

- `crates/pgvisor-sidecar/src/supervisor.rs` = `restore_from_snapshot` clearing `primary_conninfo`, deleting residual `standby.signal` and `postgresql.auto.conf`, and reaping `active_child` in `stop()` / `fence()`
- `crates/pgvisor-sidecar/src/main.rs` = Quorum auto-promotion clearing `cfg.primary_conninfo = None` and `handle_restore` setting role to leader and clearing `primary_conninfo`
- `crates/pgvisor-proxy/src/main.rs` = Replication lag monitor using `CASE WHEN NOT pg_is_in_recovery()` to prevent WAL control errors during recovery
- `crates/pgvisor-core/src/protocol/tracker.rs` = Query classifier routing `SELECT pg_switch_wal()` as `QueryKind::Write` to leader
- `tests/test-promoted-restore.sh` = Automated regression test verifying leader failover, snapshot restore on promoted leader, absence of recovery spam, and standby re-sync

### If you want to modify backup retention policies, CRON scheduling, concurrent backup/restore mutex locks, or follower backup offloading, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupScheduleConfig` (`keep_count`, `retention_days`, `full_backup_cron`, `incremental_backup_cron`, `cron_enabled`, `from_env`), `BasebackupMeta.source_node`, and `prune_retention` with single latest snapshot safeguard
- `crates/pgvisor-proxy/src/scheduler.rs` = `BackupScheduler` driving automated background full and incremental backup jobs via `croner::Cron`
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::select_backup_target` prioritizing follower/standby nodes over primary for `pg_basebackup`, and `operation_lock` (async mutex) rejecting concurrent backup/restore with error
- `crates/pgvisor-dashboard/src/handlers.rs` = `api_create_backup`, `api_restore_backup`, and `api_quick_restore` mapping concurrent lock rejections to HTTP 409 Conflict, and `StandaloneBackupService` with mutex lock
- `crates/pgvisor-dashboard/src/models.rs` = `BackupOverviewSummary.keep_count` and `BackupItemView.source_node`
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI displaying keep count in summary and source node badge in backup snapshot list
- `crates/pgvisor-proxy/src/main.rs` = Wiring `BackupScheduleConfig::from_env()` and starting `BackupScheduler`
- `docker-compose.yml` = Canonical compose definition configuring `PGVISOR_BACKUP_*` retention and CRON variables
- `examples/docker-compose.yml` = Example compose definition passing `PGVISOR_BACKUP_*` retention and CRON variables with defaults
- `examples/.env.example` & `examples/.env` = Environment variable templates defining default retention limits and CRON schedules
- `tests/test-backup-restore.sh` = Automated integration test asserting follower node backup execution, primary node restore execution, and HTTP 409 Conflict mutex rejection
