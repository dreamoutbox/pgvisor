use serde::{Deserialize, Serialize};

/// Summary information for a database table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSummary {
    pub name: String,
    pub schema: String,
    pub estimated_rows: u64,
    pub size_pretty: String,
}
