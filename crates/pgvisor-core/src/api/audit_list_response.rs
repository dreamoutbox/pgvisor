use serde::{Deserialize, Serialize};

use super::audit_event_view::AuditEventView;

/// Paginated audit list response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditListResponse {
    pub events: Vec<AuditEventView>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub dangerous_sql_count: usize,
    pub latest_pitr_target: Option<String>,
}
