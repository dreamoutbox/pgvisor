use serde::{Deserialize, Serialize};

/// Aggregated backup metrics for the dashboard summary cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupOverviewSummary {
    pub total_backups: usize,
    pub latest_backup: Option<String>,
    pub total_size_pretty: String,
    pub retention_days: u32,
    pub keep_count: Option<usize>,
    pub storage_endpoint: String,
    pub storage_bucket: String,
}
