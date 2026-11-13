use serde::{Deserialize, Serialize};

use super::table_privilege_kind::TablePrivilegeKind;

/// Request to grant or revoke a table-level privilege.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePrivilegeRequest {
    pub table_name: String,
    pub schema: Option<String>,
    pub privilege: TablePrivilegeKind,
    pub grant: bool,
}
