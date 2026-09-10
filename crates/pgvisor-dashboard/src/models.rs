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
    pub label: Option<String>,
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

/// Request body for initiating manual leader switchover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchoverRequest {
    pub target_node_id: u64,
}

/// Response payload from a successful cluster switchover operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchoverResponse {
    pub status: String,
    pub message: String,
    pub previous_leader_id: Option<u64>,
    pub new_leader_id: u64,
}

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

/// A PostgreSQL role as returned by pg_roles (excluding system pg_* roles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgRole {
    pub rolname: String,
    pub rolcanlogin: bool,
    pub rolcreatedb: bool,
    pub rolcreaterole: bool,
    pub rolreplication: bool,
    pub rolsuper: bool,
    pub rolconnlimit: i32,
    pub member_of: Vec<String>,
}

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

fn default_true() -> bool {
    true
}

fn default_conn_limit() -> i32 {
    -1
}

/// Request to create a new database role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRoleRequest {
    pub name: String,
    pub password: Option<String>,
    #[serde(default = "default_true")]
    pub login: bool,
    #[serde(default)]
    pub createdb: bool,
    #[serde(default)]
    pub createrole: bool,
    #[serde(default)]
    pub replication: bool,
    #[serde(default = "default_conn_limit")]
    pub connection_limit: i32,
}

/// Request to alter attributes of an existing role.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlterRoleRequest {
    pub password: Option<String>,
    pub login: Option<bool>,
    pub createdb: Option<bool>,
    pub createrole: Option<bool>,
    pub replication: Option<bool>,
    pub connection_limit: Option<i32>,
}

/// Request to grant or revoke role membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleMembershipRequest {
    pub member_role: String,
    pub group_role: String,
}

/// Request to grant or revoke a table-level privilege.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePrivilegeRequest {
    pub table_name: String,
    pub schema: Option<String>,
    pub privilege: TablePrivilegeKind,
    pub grant: bool,
}

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

/// Aggregated metrics for audit dashboard headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditOverviewStats {
    pub total_events: usize,
    pub dangerous_sql_count: usize,
    pub latest_pitr_target: Option<String>,
}
