use serde::{Deserialize, Serialize};

/// Response payload from a successful cluster switchover operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchoverResponse {
    pub status: String,
    pub message: String,
    pub previous_leader_id: Option<u64>,
    pub new_leader_id: u64,
}
