use serde::{Deserialize, Serialize};

/// Operational state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeHealthState {
    Healthy,
    Degraded,
    Fenced,
    Stopped,
    Offline,
}
