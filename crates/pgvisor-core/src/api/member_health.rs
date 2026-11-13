use serde::{Deserialize, Serialize};

/// Reachability state of a cluster sidecar member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberHealth {
    Healthy,
    Unreachable,
}
