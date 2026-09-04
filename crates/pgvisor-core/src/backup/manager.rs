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
}

/// Metadata describing a physical basebackup snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BasebackupMeta {
    pub snapshot_id: String,
    pub created_at: DateTime<Utc>,
    pub start_wal: String,
    pub stop_wal: Option<String>,
    pub total_bytes: u64,
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

    /// Prunes older basebackup snapshots and obsolete WAL segments older than the oldest retained basebackup.
    pub async fn prune_retention(&self, keep_count: usize) -> Result<usize, BackupError> {
        let mut snapshots = self.list_basebackups().await?;
        if snapshots.len() <= keep_count {
            return Ok(0);
        }

        let delete_count = snapshots.len() - keep_count;
        let snapshots_to_delete: Vec<BasebackupMeta> = snapshots.drain(..delete_count).collect();

        for snap in &snapshots_to_delete {
            let tar_key = format!("{}{}.tar.gz", self.basebackup_prefix(), snap.snapshot_id);
            let meta_key = format!("{}{}.json", self.basebackup_prefix(), snap.snapshot_id);
            let _ = self.operator.delete(&tar_key).await;
            let _ = self.operator.delete(&meta_key).await;
            info!(snapshot_id = %snap.snapshot_id, "Pruned expired basebackup snapshot");
        }

        // The oldest remaining snapshot determines WAL cutoff
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
            start_wal: "000000010000000000000001".into(),
            stop_wal: Some("000000010000000000000002".into()),
            total_bytes: 1024,
        };

        let meta2 = BasebackupMeta {
            snapshot_id: "snap-2".into(),
            created_at: Utc::now() - chrono::Duration::hours(1),
            start_wal: "000000010000000000000003".into(),
            stop_wal: Some("000000010000000000000004".into()),
            total_bytes: 2048,
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
        assert_eq!(list[1].snapshot_id, "snap-2");

        // Prune retention: keep 1 -> snap-1 should be pruned
        let pruned = manager.prune_retention(1).await.unwrap();
        assert_eq!(pruned, 1);

        let remaining = manager.list_basebackups().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].snapshot_id, "snap-2");
    }
}
