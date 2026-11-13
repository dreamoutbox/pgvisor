use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_conn_limit() -> i32 {
    -1
}

/// Request to create a new database role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRoleRequest {
    pub name: String,
    pub password: Option<String>,
    #[serde(default = "default_true")]
    pub login: bool,
    #[serde(default)]
    pub createdb: bool,
    #[serde(default)]
    pub createrole: bool,
    #[serde(default)]
    pub replication: bool,
    #[serde(default = "default_conn_limit")]
    pub connection_limit: i32,
}
