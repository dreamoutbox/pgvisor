use serde::{Deserialize, Serialize};

/// Request body for initiating manual leader switchover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchoverRequest {
    pub target_node_id: u64,
}
