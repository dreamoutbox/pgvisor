use serde::{Deserialize, Serialize};

use super::{MemberHealth, SidecarRole};

/// Description of one sidecar node returned by `GET /api/cluster/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterMember {
    /// Numeric node identifier when known from status or peer hostname.
    pub node_id: Option<u64>,
    /// Client API base URL (`http://host:api_port`) when reachable or configured.
    pub api_url: Option<String>,
    /// Operational role (`leader`, `standby`, `fenced`) when reachable.
    pub role: Option<SidecarRole>,
    /// Reachability status of the sidecar node.
    pub health: MemberHealth,
    /// True if this entry describes the sidecar handling the request.
    pub is_self: bool,
}
