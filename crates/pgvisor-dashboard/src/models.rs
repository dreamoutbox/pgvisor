use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Role of a node within the Raft + PostgreSQL cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Leader,
    Standby,
    Learner,
}

/// Operational state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeHealthState {
    Healthy,
    Degraded,
    Fenced,
    Offline,
}

/// Snapshot metrics and status of a cluster node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: u64,
    pub address: String,
    pub role: NodeRole,
    pub state: NodeHealthState,
    pub pg_version: String,
    pub replication_lag_bytes: u64,
    pub uptime_secs: u64,
    pub is_local: bool,
}

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

/// Request body for executing guarded SQL queries from the console.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlQueryRequest {
    pub query: String,
    pub max_rows: Option<usize>,
}

/// Result of a successfully executed console SQL query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub execution_time_ms: u64,
    pub row_count: usize,
    pub truncated: bool,
}

/// Error returned when an SQL query fails validation or execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlQueryError {
    pub code: String,
    pub message: String,
}
