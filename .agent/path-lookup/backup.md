### If you want to modify PostgreSQL WAL archiving, basebackup snapshots, or OpenDAL storage, then check:

- `crates/pgvisor-core/src/backup/manager.rs` = `BackupManager` managing OpenDAL uploads, WAL archiving/restoration, basebackup snapshotting, and retention pruning
- `crates/pgvisor-core/src/backup/mod.rs` = Backup module exports, `BasebackupMeta`, and `BackupError`
- `docker-compose.yml` = Local MinIO and S3 credentials configuration for local development
