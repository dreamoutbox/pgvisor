use serde::{Deserialize, Serialize};

/// A table-level privilege entry for a role on a specific table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TablePrivilege {
    pub table_name: String,
    pub schema: String,
    pub select: bool,
    pub insert: bool,
    pub update: bool,
    pub delete: bool,
    pub truncate: bool,
    pub references: bool,
    pub trigger: bool,
}
