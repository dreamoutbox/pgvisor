use serde::{Deserialize, Serialize};

/// Request payload for Postgres-backed sidecar API login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}
