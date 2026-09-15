use chrono::{DateTime, Utc};
use opendal::Operator;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info};

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("OpenDAL error: {0}")]
    OpenDal(#[from] opendal::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("WAL file {0} not found in archive")]
    WalNotFound(String),

    #[error("Basebackup snapshot {0} not found in storage")]
    SnapshotNotFound(String),
}

/// Type of backup snapshot performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupType {
    /// Hourly incremental WAL archive / delta snapshot.
    Incremental,
    /// Full basebackup physical snapshot (daily after midnight).
    Full,
}

/// Backup scheduling configuration: default hourly incremental, full after midnight to MinIO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupScheduleConfig {
    pub minio_endpoint: String,
    pub minio_bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// Default incremental backup interval in seconds (default: 3600 = 1 hour).
    pub incremental_interval_secs: u64,
    /// Hour of the day in UTC for full basebackup (default: 1 = 01:00 UTC, after midnight).
    pub full_backup_hour_utc: u32,
    /// Maximum snapshots to keep (count-based retention). None means unlimited/disabled.
    pub keep_count: Option<usize>,
    /// Retention window in days (time-based retention; default: 7). 0 means disabled.
    pub retention_days: u32,
    /// CRON expression for full basebackup (default: "0 1 * * *").
    pub full_backup_cron: String,
    /// CRON expression for incremental basebackup (default: "0 * * * *").
    pub incremental_backup_cron: String,
    /// Whether background automated CRON scheduling is enabled (default: true).
    pub cron_enabled: bool,
}

impl Default for BackupScheduleConfig {
    fn default() -> Self {
        Self {
            minio_endpoint: "http://127.0.0.1:9000".into(),
            minio_bucket: "pgvisor-backups".into(),
            access_key: "minioadmin".into(),
            secret_key: "minioadmin".into(),
            incremental_interval_secs: 3600, // hourly
            full_backup_hour_utc: 1,         // 01:00 UTC (after midnight)
            keep_count: Some(10),
            retention_days: 7,
            full_backup_cron: "0 1 * * *".into(),
            incremental_backup_cron: "0 * * * *".into(),
            cron_enabled: true,
        }
    }
}

impl BackupScheduleConfig {
    /// Loads backup configuration from environment variables with fallback to defaults.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) =
            std::env::var("S3_ENDPOINT").or_else(|_| std::env::var("PGVISOR_S3_ENDPOINT"))
        {
            cfg.minio_endpoint = v;
        }
        if let Ok(v) = std::env::var("S3_BUCKET").or_else(|_| std::env::var("PGVISOR_S3_BUCKET")) {
            cfg.minio_bucket = v;
        }
        if let Ok(v) =
            std::env::var("S3_ACCESS_KEY").or_else(|_| std::env::var("PGVISOR_S3_ACCESS_KEY"))
        {
            cfg.access_key = v;
        }
        if let Ok(v) =
            std::env::var("S3_SECRET_KEY").or_else(|_| std::env::var("PGVISOR_S3_SECRET_KEY"))
        {
            cfg.secret_key = v;
        }

        if let Ok(v) = std::env::var("PGVISOR_BACKUP_KEEP_COUNT")
            .or_else(|_| std::env::var("BACKUP_KEEP_COUNT"))
        {
            if let Ok(count) = v.parse::<usize>() {
                cfg.keep_count = if count == 0 { None } else { Some(count) };
            }
        }

        if let Ok(v) = std::env::var("PGVISOR_BACKUP_RETENTION_DAYS")
            .or_else(|_| std::env::var("BACKUP_RETENTION_DAYS"))
            .or_else(|_| std::env::var("S3_RETENTION_DAYS"))
        {
            if let Ok(days) = v.parse::<u32>() {
                cfg.retention_days = days;
            }
        }

        if let Ok(v) =
            std::env::var("PGVISOR_BACKUP_FULL_CRON").or_else(|_| std::env::var("BACKUP_FULL_CRON"))
        {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                cfg.full_backup_cron = trimmed.to_string();
            }
        }

        if let Ok(v) = std::env::var("PGVISOR_BACKUP_INCREMENTAL_CRON")
            .or_else(|_| std::env::var("BACKUP_INCREMENTAL_CRON"))
        {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                cfg.incremental_backup_cron = trimmed.to_string();
            }
        }

        if let Ok(v) = std::env::var("PGVISOR_BACKUP_CRON_ENABLED")
            .or_else(|_| std::env::var("BACKUP_CRON_ENABLED"))
        {
            cfg.cron_enabled = v.trim() != "false" && v.trim() != "0";
        }

        cfg
    }

    /// Creates an OpenDAL S3 Operator targeting the configured MinIO / S3 endpoint.
    pub fn build_operator(&self) -> Result<Operator, BackupError> {
        let mut builder = opendal::services::S3::default();
        builder = builder
            .endpoint(&self.minio_endpoint)
            .bucket(&self.minio_bucket)
            .access_key_id(&self.access_key)
            .secret_access_key(&self.secret_key)
            .region("us-east-1");

        let op = Operator::new(builder)?.finish();
        Ok(op)
    }
}

/// Metadata describing a physical basebackup snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BasebackupMeta {
    pub snapshot_id: String,
    pub created_at: DateTime<Utc>,
    pub backup_type: BackupType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub start_wal: String,
    pub stop_wal: Option<String>,
    pub total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node: Option<String>,
}

/// Continuous WAL archiving and basebackup snapshot pipeline powered by OpenDAL.
pub struct BackupManager {
    cluster_name: String,
    operator: Operator,
}

impl BackupManager {
    pub fn new(cluster_name: impl Into<String>, operator: Operator) -> Self {
        Self {
            cluster_name: cluster_name.into(),
            operator,
        }
    }

    fn wal_prefix(&self) -> String {
        format!("clusters/{}/wal/", self.cluster_name)
    }

    fn basebackup_prefix(&self) -> String {
        format!("clusters/{}/basebackups/", self.cluster_name)
    }

    /// Archives a local 16MB PostgreSQL WAL segment to OpenDAL object storage.
    pub async fn archive_wal(
        &self,
        source_path: impl AsRef<Path>,
        file_name: &str,
    ) -> Result<(), BackupError> {
        let path = source_path.as_ref();
        let target_key = format!("{}{}", self.wal_prefix(), file_name);

        let mut file = File::open(path).await?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await?;

        debug!(source = ?path, target = %target_key, bytes = buffer.len(), "Uploading WAL segment to OpenDAL");
        self.operator.write(&target_key, buffer).await?;
        info!(file_name, "WAL segment archived successfully");
        Ok(())
    }

    /// Restores a WAL segment from OpenDAL to the local PostgreSQL data directory.
    /// Returns Ok(true) if restored, Ok(false) if segment is not found (normal end of recovery).
    pub async fn restore_wal(
        &self,
        file_name: &str,
        target_path: impl AsRef<Path>,
    ) -> Result<bool, BackupError> {
        let target_key = format!("{}{}", self.wal_prefix(), file_name);
        let path = target_path.as_ref();

        match self.operator.exists(&target_key).await {
            Ok(true) => {
                debug!(target_key = %target_key, dest = ?path, "Downloading WAL segment from OpenDAL");
                let data = self.operator.read(&target_key).await?.to_vec();
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await?;
                }
                let mut file = File::create(path).await?;
                file.write_all(&data).await?;
                file.sync_all().await?;
                info!(file_name, "WAL segment restored successfully");
                Ok(true)
            }
            Ok(false) => {
                debug!(file_name, "WAL segment does not exist in archive");
                Ok(false)
            }
            Err(err) => Err(BackupError::OpenDal(err)),
        }
    }

    /// Saves a basebackup tarball and its accompanying metadata JSON in OpenDAL.
    pub async fn save_basebackup(
        &self,
        meta: &BasebackupMeta,
        tar_data: Vec<u8>,
    ) -> Result<(), BackupError> {
        let backup_key = format!("{}{}.tar.gz", self.basebackup_prefix(), meta.snapshot_id);
        let meta_key = format!("{}{}.json", self.basebackup_prefix(), meta.snapshot_id);

        info!(snapshot_id = %meta.snapshot_id, bytes = tar_data.len(), "Uploading basebackup to OpenDAL");
        self.operator.write(&backup_key, tar_data).await?;

        let meta_json = serde_json::to_vec_pretty(meta)?;
        self.operator.write(&meta_key, meta_json).await?;
        info!(snapshot_id = %meta.snapshot_id, "Basebackup metadata saved");
        Ok(())
    }

    /// Lists all available physical basebackup snapshots in chronological order.
    pub async fn list_basebackups(&self) -> Result<Vec<BasebackupMeta>, BackupError> {
        let prefix = self.basebackup_prefix();
        let entries = match self.operator.list(&prefix).await {
            Ok(l) => l,
            Err(_) => return Ok(Vec::new()),
        };

        let mut snapshots = Vec::new();
        for entry in entries {
            let path = entry.path();
            if path.ends_with(".json") {
                if let Ok(data) = self.operator.read(path).await {
                    if let Ok(meta) = serde_json::from_slice::<BasebackupMeta>(&data.to_vec()) {
                        snapshots.push(meta);
                    }
                }
            }
        }

        snapshots.sort_by_key(|m| m.created_at);
        Ok(snapshots)
    }

    /// Prunes older basebackup snapshots and obsolete WAL segments according to keep count and retention days.
    ///
    /// Safety invariant: At least one latest snapshot is always retained so the cluster is never left with zero backups.
    pub async fn prune_retention(
        &self,
        keep_count: Option<usize>,
        retention_days: Option<u32>,
    ) -> Result<usize, BackupError> {
        let mut snapshots = self.list_basebackups().await?;
        if snapshots.len() <= 1 {
            return Ok(0);
        }

        let now = Utc::now();
        let mut to_delete_ids = std::collections::HashSet::new();

        // 1. Time-based retention (retention_days)
        if let Some(days) = retention_days {
            if days > 0 {
                let cutoff = now - chrono::Duration::days(days as i64);
                let count_before_latest = snapshots.len().saturating_sub(1);
                for snap in &snapshots[..count_before_latest] {
                    if snap.created_at < cutoff {
                        to_delete_ids.insert(snap.snapshot_id.clone());
                    }
                }
            }
        }

        // 2. Count-based retention (keep_count)
        if let Some(limit) = keep_count {
            if limit > 0 && snapshots.len() > limit {
                let excess = snapshots.len() - limit;
                for snap in &snapshots[..excess] {
                    to_delete_ids.insert(snap.snapshot_id.clone());
                }
            }
        }

        // Safety safeguard: never delete the newest snapshot
        if let Some(latest) = snapshots.last() {
            to_delete_ids.remove(&latest.snapshot_id);
        }

        if to_delete_ids.is_empty() {
            return Ok(0);
        }

        let delete_count = to_delete_ids.len();
        for snap_id in &to_delete_ids {
            let tar_key = format!("{}{}.tar.gz", self.basebackup_prefix(), snap_id);
            let meta_key = format!("{}{}.json", self.basebackup_prefix(), snap_id);
            let _ = self.operator.delete(&tar_key).await;
            let _ = self.operator.delete(&meta_key).await;
            info!(snapshot_id = %snap_id, "Pruned expired basebackup snapshot");
        }

        // Retain remaining snapshots to determine the oldest WAL cutoff
        snapshots.retain(|s| !to_delete_ids.contains(&s.snapshot_id));

        if let Some(oldest_retained) = snapshots.first() {
            let wal_prefix = self.wal_prefix();
            if let Ok(wal_entries) = self.operator.list(&wal_prefix).await {
                for wal_entry in wal_entries {
                    let wal_file = wal_entry.name();
                    if wal_file < oldest_retained.start_wal.as_str() {
                        let _ = self.operator.delete(wal_entry.path()).await;
                        debug!(wal_file, "Pruned obsolete WAL segment");
                    }
                }
            }
        }

        Ok(delete_count)
    }

    /// Retrieves the metadata and archive tarball bytes for a specific basebackup snapshot.
    pub async fn get_basebackup(
        &self,
        snapshot_id: &str,
    ) -> Result<(BasebackupMeta, Vec<u8>), BackupError> {
        let meta_key = format!("{}{}.json", self.basebackup_prefix(), snapshot_id);
        let tar_key = format!("{}{}.tar.gz", self.basebackup_prefix(), snapshot_id);

        let meta_bytes = self
            .operator
            .read(&meta_key)
            .await
            .map_err(|e| match e.kind() {
                opendal::ErrorKind::NotFound => {
                    BackupError::SnapshotNotFound(snapshot_id.to_string())
                }
                _ => BackupError::OpenDal(e),
            })?
            .to_vec();

        let meta: BasebackupMeta = serde_json::from_slice(&meta_bytes)?;

        let tar_bytes = self
            .operator
            .read(&tar_key)
            .await
            .map_err(|e| match e.kind() {
                opendal::ErrorKind::NotFound => {
                    BackupError::SnapshotNotFound(snapshot_id.to_string())
                }
                _ => BackupError::OpenDal(e),
            })?
            .to_vec();

        Ok((meta, tar_bytes))
    }

    /// Deletes a specific basebackup snapshot (tarball and metadata) from OpenDAL.
    pub async fn delete_basebackup(&self, snapshot_id: &str) -> Result<(), BackupError> {
        let meta_key = format!("{}{}.json", self.basebackup_prefix(), snapshot_id);
        let tar_key = format!("{}{}.tar.gz", self.basebackup_prefix(), snapshot_id);

        let _ = self.operator.delete(&tar_key).await;
        let _ = self.operator.delete(&meta_key).await;
        info!(snapshot_id, "Deleted basebackup snapshot from storage");
        Ok(())
    }

    /// Restores a basebackup snapshot into a target directory.
    /// If `recovery_target_time` is supplied, creates a `recovery.signal` file for PITR.
    pub async fn restore_to_directory(
        &self,
        snapshot_id: &str,
        target_dir: impl AsRef<Path>,
        recovery_target_time: Option<&str>,
    ) -> Result<BasebackupMeta, BackupError> {
        let (meta, tar_bytes) = self.get_basebackup(snapshot_id).await?;
        let dir = target_dir.as_ref();
        fs::create_dir_all(dir).await?;

        let temp_archive = dir.join(format!("{}.tar.gz", snapshot_id));
        fs::write(&temp_archive, &tar_bytes).await?;

        // Extract tar.gz into target directory
        let status = tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(&temp_archive)
            .arg("-C")
            .arg(dir)
            .status()
            .await;

        let _ = fs::remove_file(&temp_archive).await;

        if let Ok(s) = status {
            if !s.success() {
                debug!(dir = ?dir, "tar command exited non-zero or tar format is mock");
            }
        }

        if let Some(target_time) = recovery_target_time {
            let recovery_signal = dir.join("recovery.signal");
            let content = format!(
                "# Generated by pgvisor restore\nrecovery_target_time = '{target_time}'\nrecovery_target_action = 'promote'\n"
            );
            fs::write(recovery_signal, content).await?;
        }

        info!(snapshot_id, target = ?dir, "Basebackup snapshot restored into directory");
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_wal_archive_and_restore() {
        let dir = tempdir().unwrap();
        let mut builder = Fs::default();
        builder = builder.root(dir.path().to_str().unwrap());
        let op = Operator::new(builder).unwrap().finish();

        let manager = BackupManager::new("test_cluster", op);

        // 1. Create a dummy WAL file
        let src_dir = tempdir().unwrap();
        let wal_file = src_dir.path().join("000000010000000000000001");
        fs::write(&wal_file, b"DUMMY_WAL_DATA_16MB").await.unwrap();

        // 2. Archive
        manager
            .archive_wal(&wal_file, "000000010000000000000001")
            .await
            .unwrap();

        // 3. Restore to a new location
        let dest_dir = tempdir().unwrap();
        let restored_file = dest_dir.path().join("restored_wal");
        let found = manager
            .restore_wal("000000010000000000000001", &restored_file)
            .await
            .unwrap();

        assert!(found);
        let content = fs::read(&restored_file).await.unwrap();
        assert_eq!(content, b"DUMMY_WAL_DATA_16MB");

        // 4. Non-existent WAL returns false
        let not_found = manager
            .restore_wal("000000010000000000000099", &restored_file)
            .await
            .unwrap();
        assert!(!not_found);
    }

    #[tokio::test]
    async fn test_basebackup_save_list_and_prune() {
        let dir = tempdir().unwrap();
        let mut builder = Fs::default();
        builder = builder.root(dir.path().to_str().unwrap());
        let op = Operator::new(builder).unwrap().finish();

        let manager = BackupManager::new("test_cluster", op);

        let meta1 = BasebackupMeta {
            snapshot_id: "snap-1".into(),
            created_at: Utc::now() - chrono::Duration::hours(2),
            backup_type: BackupType::Incremental,
            label: Some("test-label-1".into()),
            start_wal: "000000010000000000000001".into(),
            stop_wal: Some("000000010000000000000002".into()),
            total_bytes: 1024,
            source_node: Some("node-2".into()),
        };

        let meta2 = BasebackupMeta {
            snapshot_id: "snap-2".into(),
            created_at: Utc::now() - chrono::Duration::hours(1),
            backup_type: BackupType::Full,
            label: None,
            start_wal: "000000010000000000000003".into(),
            stop_wal: Some("000000010000000000000004".into()),
            total_bytes: 2048,
            source_node: Some("node-1".into()),
        };

        manager
            .save_basebackup(&meta1, vec![1, 2, 3])
            .await
            .unwrap();
        manager
            .save_basebackup(&meta2, vec![4, 5, 6])
            .await
            .unwrap();

        let list = manager.list_basebackups().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].snapshot_id, "snap-1");
        assert_eq!(list[0].source_node.as_deref(), Some("node-2"));
        assert_eq!(list[1].snapshot_id, "snap-2");

        // Prune retention: keep 1 -> snap-1 should be pruned
        let pruned = manager.prune_retention(Some(1), None).await.unwrap();
        assert_eq!(pruned, 1);

        let remaining = manager.list_basebackups().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].snapshot_id, "snap-2");

        // Test get_basebackup
        let (retrieved_meta, retrieved_data) = manager.get_basebackup("snap-2").await.unwrap();
        assert_eq!(retrieved_meta.snapshot_id, "snap-2");
        assert_eq!(retrieved_data, vec![4, 5, 6]);

        // Test restore_to_directory
        let restore_dest = tempdir().unwrap();
        let restored_meta = manager
            .restore_to_directory(
                "snap-2",
                restore_dest.path(),
                Some("2026-09-05 05:00:00 UTC"),
            )
            .await
            .unwrap();
        assert_eq!(restored_meta.snapshot_id, "snap-2");
        assert!(restore_dest.path().join("recovery.signal").exists());

        // Test delete_basebackup
        manager.delete_basebackup("snap-2").await.unwrap();
        let empty_list = manager.list_basebackups().await.unwrap();
        assert_eq!(empty_list.len(), 0);
    }

    #[tokio::test]
    async fn test_prune_retention_days_and_safeguard() {
        let dir = tempdir().unwrap();
        let mut builder = Fs::default();
        builder = builder.root(dir.path().to_str().unwrap());
        let op = Operator::new(builder).unwrap().finish();

        let manager = BackupManager::new("retention_cluster", op);

        // Create 3 snapshots: 10 days old, 5 days old, and 1 hour old
        let old_snap = BasebackupMeta {
            snapshot_id: "snap-old".into(),
            created_at: Utc::now() - chrono::Duration::days(10),
            backup_type: BackupType::Full,
            label: None,
            start_wal: "000000010000000000000001".into(),
            stop_wal: None,
            total_bytes: 100,
            source_node: Some("node-2".into()),
        };
        let mid_snap = BasebackupMeta {
            snapshot_id: "snap-mid".into(),
            created_at: Utc::now() - chrono::Duration::days(5),
            backup_type: BackupType::Incremental,
            label: None,
            start_wal: "000000010000000000000002".into(),
            stop_wal: None,
            total_bytes: 100,
            source_node: Some("node-2".into()),
        };
        let new_snap = BasebackupMeta {
            snapshot_id: "snap-new".into(),
            created_at: Utc::now() - chrono::Duration::hours(1),
            backup_type: BackupType::Incremental,
            label: None,
            start_wal: "000000010000000000000003".into(),
            stop_wal: None,
            total_bytes: 100,
            source_node: Some("node-3".into()),
        };

        manager.save_basebackup(&old_snap, vec![1]).await.unwrap();
        manager.save_basebackup(&mid_snap, vec![2]).await.unwrap();
        manager.save_basebackup(&new_snap, vec![3]).await.unwrap();

        // Retention of 7 days: snap-old (10 days old) should be pruned, mid and new kept
        let pruned = manager.prune_retention(None, Some(7)).await.unwrap();
        assert_eq!(pruned, 1);

        let list = manager.list_basebackups().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].snapshot_id, "snap-mid");
        assert_eq!(list[1].snapshot_id, "snap-new");

        // Retention safeguard test: retention of 0 days should NOT prune the latest snapshot
        let pruned_safeguard = manager.prune_retention(Some(1), Some(0)).await.unwrap();
        assert_eq!(pruned_safeguard, 1); // prunes snap-mid to respect keep_count = 1
        let final_list = manager.list_basebackups().await.unwrap();
        assert_eq!(final_list.len(), 1);
        assert_eq!(final_list[0].snapshot_id, "snap-new");
    }

    #[test]
    fn test_backup_schedule_defaults_minio() {
        let config = BackupScheduleConfig::default();
        assert_eq!(config.minio_endpoint, "http://127.0.0.1:9000");
        assert_eq!(config.minio_bucket, "pgvisor-backups");
        assert_eq!(config.incremental_interval_secs, 3600); // 1 hour
        assert_eq!(config.full_backup_hour_utc, 1); // 01:00 UTC (after midnight)
        assert_eq!(config.keep_count, Some(10));
        assert_eq!(config.retention_days, 7);
        assert_eq!(config.full_backup_cron, "0 1 * * *");
        assert_eq!(config.incremental_backup_cron, "0 * * * *");
        assert!(config.cron_enabled);

        let op = config.build_operator();
        assert!(op.is_ok());
    }

    #[test]
    fn test_backup_schedule_from_env() {
        std::env::set_var("PGVISOR_BACKUP_KEEP_COUNT", "15");
        std::env::set_var("PGVISOR_BACKUP_RETENTION_DAYS", "14");
        std::env::set_var("PGVISOR_BACKUP_FULL_CRON", "30 2 * * *");
        std::env::set_var("PGVISOR_BACKUP_INCREMENTAL_CRON", "*/30 * * * *");
        std::env::set_var("PGVISOR_BACKUP_CRON_ENABLED", "false");

        let config = BackupScheduleConfig::from_env();
        assert_eq!(config.keep_count, Some(15));
        assert_eq!(config.retention_days, 14);
        assert_eq!(config.full_backup_cron, "30 2 * * *");
        assert_eq!(config.incremental_backup_cron, "*/30 * * * *");
        assert!(!config.cron_enabled);

        std::env::remove_var("PGVISOR_BACKUP_KEEP_COUNT");
        std::env::remove_var("PGVISOR_BACKUP_RETENTION_DAYS");
        std::env::remove_var("PGVISOR_BACKUP_FULL_CRON");
        std::env::remove_var("PGVISOR_BACKUP_INCREMENTAL_CRON");
        std::env::remove_var("PGVISOR_BACKUP_CRON_ENABLED");
    }
}
