use serde::{Deserialize, Serialize};

/// Request to grant or revoke role membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleMembershipRequest {
    pub member_role: String,
    pub group_role: String,
}
