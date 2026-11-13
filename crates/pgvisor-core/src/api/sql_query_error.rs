use serde::{Deserialize, Serialize};

/// Error returned when an SQL query fails validation or execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlQueryError {
    pub code: String,
    pub message: String,
}
