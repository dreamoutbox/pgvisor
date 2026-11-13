use serde::{Deserialize, Serialize};

/// PITR valid time window and timestamp boundaries for slider synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PitrWindowResponse {
    pub snapshot_id: String,
    pub min_time: String,
    pub min_timestamp: i64,
    pub max_time: String,
    pub max_timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transaction_timestamp: Option<i64>,
}
