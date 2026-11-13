use serde::{Deserialize, Serialize};

/// Authenticated principal information returned by `GET /api/auth/me`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    pub username: String,
}
