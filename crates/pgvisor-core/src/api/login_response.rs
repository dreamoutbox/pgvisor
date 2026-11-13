use serde::{Deserialize, Serialize};

/// Response returned after successful Postgres-user authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub username: String,
    pub expires_at: i64,
}
