### If you want to modify PostgreSQL WAL archiving, basebackup snapshots, or OpenDAL storage, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupManager` managing OpenDAL uploads, WAL archiving/restoration, basebackup snapshotting, `BackupScheduleConfig` (MinIO dev target, hourly incremental, midnight full backup), retention pruning, and snapshot retrieval/deletion
- `crates/pgvisor-core/src/backup/mod.rs` = Backup module exports, `BasebackupMeta`, `BackupType`, `BackupScheduleConfig`, and `BackupError`
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService` orchestrating live physical snapshots via `pg_basebackup`, cluster restore coordination via leader sidecar `/control/restore` and standby `/control/resync`, and connection pool draining
- `crates/pgvisor-sidecar/src/supervisor.rs` = `PostgresSupervisor::restore_from_snapshot` and `resync_from_primary` for in-place cluster data directory restoration, standby re-cloning, and PITR recovery signaling
- `knowledges/post-mortem-flaky-pitr-restore.md` = Post-mortem detailing cluster restore timeline, split-brain failover race conditions under parallel CPU load, and resolution
- `docker-compose.yml` = Local MinIO and S3 credentials configuration for local development

### If you want to modify backup metadata (labels, notes, WAL ranges), snapshot triggering, or backup table views, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BasebackupMeta` metadata struct (snapshot ID, label, created timestamp, backup type, WAL range, byte size) and OpenDAL storage manager
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::create_backup` passing optional label to `BasebackupMeta` and executing `pg_basebackup`
- `crates/pgvisor-dashboard/src/models.rs` = `CreateBackupRequest` and `BackupItemView` view models
- `crates/pgvisor-dashboard/src/handlers.rs` = Backup creation and list endpoints (`/api/backups`) and page renderer mapping metadata to `BackupItemView`
- `crates/pgvisor-dashboard/templates/backups.html` = Dashboard UI table displaying backup items (Snapshot ID, Type, Optional Label / Note, Created At, Size, WAL range, actions) and creation modal
