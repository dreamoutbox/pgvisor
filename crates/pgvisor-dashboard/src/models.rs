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

/// Summary information for a database table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSummary {
    pub name: String,
    pub schema: String,
    pub estimated_rows: u64,
    pub size_pretty: String,
}

/// Metadata description of a table column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub default_value: Option<String>,
    pub is_primary_key: bool,
}

/// Paginated table data response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDataResponse {
    pub table_name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub total_rows: u64,
    pub limit: usize,
    pub offset: usize,
}

/// Request to trigger a physical backup snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateBackupRequest {
    pub backup_type: Option<pgvisor_core::backup::BackupType>,
    pub label: Option<String>,
}

/// Request to restore cluster from a specific snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreBackupRequest {
    pub recovery_target_time: Option<String>,
}

/// Enriched view item for backup display in the dashboard table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupItemView {
    pub snapshot_id: String,
    pub created_at: String,
    pub backup_type: String,
    pub start_wal: String,
    pub stop_wal: String,
    pub size_pretty: String,
    pub total_bytes: u64,
}

/// Aggregated backup metrics for the dashboard summary cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupOverviewSummary {
    pub total_backups: usize,
    pub latest_backup: Option<String>,
    pub total_size_pretty: String,
    pub retention_days: u32,
    pub storage_endpoint: String,
    pub storage_bucket: String,
}

/// Formats raw byte count into human-readable representation.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
