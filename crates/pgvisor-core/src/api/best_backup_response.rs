use serde::{Deserialize, Serialize};

/// Information about the resolved best backup for a target timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BestBackupResponse {
    pub snapshot_id: String,
    pub created_at: String,
    pub backup_type: String,
    pub label: Option<String>,
}
