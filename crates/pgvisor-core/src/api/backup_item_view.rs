use serde::{Deserialize, Serialize};

use super::format::format_bytes;
use crate::backup::{BackupType, BasebackupMeta};

/// Enriched view item for backup display in the dashboard table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupItemView {
    pub snapshot_id: String,
    pub created_at: String,
    pub backup_type: String,
    pub label: Option<String>,
    pub start_wal: String,
    pub stop_wal: String,
    pub size_pretty: String,
    pub total_bytes: u64,
    pub source_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_xid: Option<String>,
}

impl BackupItemView {
    /// Maps a [`BasebackupMeta`] domain model into an enriched view item.
    ///
    /// # Arguments
    /// * `meta` - Reference to the basebackup metadata record.
    ///
    /// # Example
    /// ```
    /// use chrono::Utc;
    /// use pgvisor_core::api::BackupItemView;
    /// use pgvisor_core::backup::{BackupType, BasebackupMeta};
    ///
    /// let meta = BasebackupMeta::new("snap-1", Utc::now(), BackupType::Full, "0/1", 1024)
    ///     .with_last_transaction(Some(Utc::now()), Some("42".to_string()));
    /// let view = BackupItemView::from_meta(&meta);
    /// assert_eq!(view.snapshot_id, "snap-1");
    /// assert_eq!(view.last_transaction_xid.as_deref(), Some("42"));
    /// ```
    pub fn from_meta(meta: &BasebackupMeta) -> Self {
        let backup_type = match meta.backup_type {
            BackupType::Full => "full".to_string(),
            BackupType::Incremental => "incremental".to_string(),
        };
        Self {
            snapshot_id: meta.snapshot_id.clone(),
            created_at: meta.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            backup_type,
            label: meta.label.clone(),
            start_wal: meta.start_wal.clone(),
            stop_wal: meta.stop_wal.clone().unwrap_or_else(|| "-".to_string()),
            size_pretty: format_bytes(meta.total_bytes),
            total_bytes: meta.total_bytes,
            source_node: meta.source_node.clone(),
            last_transaction_time: meta
                .last_transaction_time
                .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
            last_transaction_xid: meta.last_transaction_xid.clone(),
        }
    }
}
