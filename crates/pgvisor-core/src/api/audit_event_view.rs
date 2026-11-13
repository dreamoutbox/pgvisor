use serde::{Deserialize, Serialize};

/// Presentational representation of an audit event for dashboard views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventView {
    pub id: u64,
    pub occurred_at: String,
    pub kind: String,
    pub kind_display: String,
    pub node_id: Option<u64>,
    pub node_address: Option<String>,
    pub detail: String,
    pub pitr_target: Option<String>,
}
