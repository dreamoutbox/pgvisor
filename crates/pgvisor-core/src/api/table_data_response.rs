use serde::{Deserialize, Serialize};

/// Paginated table data response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDataResponse {
    pub table_name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub primary_keys: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub total_rows: u64,
    pub limit: usize,
    pub offset: usize,
}
