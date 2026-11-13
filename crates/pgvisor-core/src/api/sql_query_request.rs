use serde::{Deserialize, Serialize};

/// Request body for executing guarded SQL queries from the console.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlQueryRequest {
    pub query: String,
    pub max_rows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}
