use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::node_summary::NodeSummary;

/// Aggregated cluster health and consensus status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterOverview {
    pub cluster_id: String,
    pub current_term: u64,
    pub leader_id: Option<u64>,
    pub leader_address: Option<String>,
    pub quorum_size: usize,
    pub total_nodes: usize,
    pub healthy_nodes: usize,
    pub last_backup_at: Option<DateTime<Utc>>,
    pub total_backups: usize,
    pub nodes: Vec<NodeSummary>,
}
