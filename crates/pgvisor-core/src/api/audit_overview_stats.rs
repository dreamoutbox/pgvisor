use serde::{Deserialize, Serialize};

/// Aggregated metrics for audit dashboard headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditOverviewStats {
    pub total_events: usize,
    pub dangerous_sql_count: usize,
    pub latest_pitr_target: Option<String>,
}
