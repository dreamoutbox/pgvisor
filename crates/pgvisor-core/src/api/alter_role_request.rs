use serde::{Deserialize, Serialize};

/// Request to alter attributes of an existing role.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlterRoleRequest {
    pub password: Option<String>,
    pub login: Option<bool>,
    pub createdb: Option<bool>,
    pub createrole: Option<bool>,
    pub replication: Option<bool>,
    pub connection_limit: Option<i32>,
}
