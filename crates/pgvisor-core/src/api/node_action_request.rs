use serde::{Deserialize, Serialize};

use super::node_lifecycle_action::NodeLifecycleAction;

/// Request payload to perform a lifecycle action on a specific node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeActionRequest {
    pub action: NodeLifecycleAction,
}
