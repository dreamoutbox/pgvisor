use serde::{Deserialize, Serialize};

/// Response returned after successfully deleting or updating a table row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowMutationResponse {
    pub status: String,
    pub message: String,
    pub affected_rows: usize,
}
