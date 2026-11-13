use serde::{Deserialize, Serialize};

use super::ClusterMember;

/// Response payload for `GET /api/cluster/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterMembersResponse {
    /// API base URL of the current healthy leader, or `None` if no leader is available.
    pub leader_url: Option<String>,
    /// All known sidecar members in the cluster.
    pub members: Vec<ClusterMember>,
}
