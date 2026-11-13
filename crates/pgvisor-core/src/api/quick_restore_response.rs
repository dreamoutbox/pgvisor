use serde::{Deserialize, Serialize};

/// Result returned from a quick restore operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickRestoreResponse {
    pub status: String,
    pub message: String,
    pub snapshot_id: String,
    pub recovery_target_time: String,
    pub snapshot_created_at: String,
}
