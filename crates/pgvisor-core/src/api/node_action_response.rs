use serde::{Deserialize, Serialize};

use super::node_lifecycle_action::NodeLifecycleAction;

/// Response payload from a successful node lifecycle action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeActionResponse {
    pub status: String,
    pub message: String,
    pub node_id: u64,
    pub action: NodeLifecycleAction,
}
