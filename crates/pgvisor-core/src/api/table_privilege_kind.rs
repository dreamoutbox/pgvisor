use serde::{Deserialize, Serialize};

/// Closed set of supported table-level privilege types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TablePrivilegeKind {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
}

impl TablePrivilegeKind {
    pub fn as_sql_str(&self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::References => "REFERENCES",
            Self::Trigger => "TRIGGER",
        }
    }
}
