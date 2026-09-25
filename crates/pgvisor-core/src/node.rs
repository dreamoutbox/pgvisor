use serde::{Deserialize, Serialize};

/// Enumeration of inspectable PostgreSQL configuration, diagnostic, and state files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeConfigType {
    PostgresqlConf,
    PostgresqlAutoConf,
    PgHbaConf,
    PgIdentConf,
    PostmasterPid,
    PostmasterOpts,
    StandbySignal,
    RecoverySignal,
    BackupLabel,
}

impl NodeConfigType {
    /// Safe relative filename within `$PGDATA`.
    pub fn filename(&self) -> &'static str {
        match self {
            Self::PostgresqlConf => "postgresql.conf",
            Self::PostgresqlAutoConf => "postgresql.auto.conf",
            Self::PgHbaConf => "pg_hba.conf",
            Self::PgIdentConf => "pg_ident.conf",
            Self::PostmasterPid => "postmaster.pid",
            Self::PostmasterOpts => "postmaster.opts",
            Self::StandbySignal => "standby.signal",
            Self::RecoverySignal => "recovery.signal",
            Self::BackupLabel => "backup_label",
        }
    }

    /// Human-friendly display title for dashboard tabs.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PostgresqlConf => "postgresql.conf (Base Config)",
            Self::PostgresqlAutoConf => "postgresql.auto.conf (Auto / Replication)",
            Self::PgHbaConf => "pg_hba.conf (Client Auth)",
            Self::PgIdentConf => "pg_ident.conf (Ident Map)",
            Self::PostmasterPid => "postmaster.pid (Process Status)",
            Self::PostmasterOpts => "postmaster.opts (Start Flags)",
            Self::StandbySignal => "standby.signal (Replica State)",
            Self::RecoverySignal => "recovery.signal (PITR State)",
            Self::BackupLabel => "backup_label (Checkpoint LSN)",
        }
    }

    /// Parse config type from slug/string identifier.
    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug.trim().to_lowercase().as_str() {
            "postgresql.conf" | "postgresql_conf" => Some(Self::PostgresqlConf),
            "postgresql.auto.conf" | "postgresql_auto_conf" => Some(Self::PostgresqlAutoConf),
            "pg_hba.conf" | "pg_hba_conf" => Some(Self::PgHbaConf),
            "pg_ident.conf" | "pg_ident_conf" => Some(Self::PgIdentConf),
            "postmaster.pid" | "postmaster_pid" => Some(Self::PostmasterPid),
            "postmaster.opts" | "postmaster_opts" => Some(Self::PostmasterOpts),
            "standby.signal" | "standby_signal" => Some(Self::StandbySignal),
            "recovery.signal" | "recovery_signal" => Some(Self::RecoverySignal),
            "backup_label" => Some(Self::BackupLabel),
            _ => None,
        }
    }

    /// Canonical slug used for URL parameters.
    pub fn to_slug(&self) -> &'static str {
        match self {
            Self::PostgresqlConf => "postgresql_conf",
            Self::PostgresqlAutoConf => "postgresql_auto_conf",
            Self::PgHbaConf => "pg_hba_conf",
            Self::PgIdentConf => "pg_ident_conf",
            Self::PostmasterPid => "postmaster_pid",
            Self::PostmasterOpts => "postmaster_opts",
            Self::StandbySignal => "standby_signal",
            Self::RecoverySignal => "recovery_signal",
            Self::BackupLabel => "backup_label",
        }
    }

    /// List of all inspectable file types.
    pub fn all() -> &'static [NodeConfigType] {
        &[
            Self::PostgresqlConf,
            Self::PostgresqlAutoConf,
            Self::PgHbaConf,
            Self::PgIdentConf,
            Self::PostmasterPid,
            Self::PostmasterOpts,
            Self::StandbySignal,
            Self::RecoverySignal,
            Self::BackupLabel,
        ]
    }
}

/// Log severity stream level for captured node logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// A single timestamped log line captured from the PostgreSQL server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLogEntry {
    pub timestamp_ms: u64,
    pub level: LogLevel,
    pub message: String,
}

/// Response payload containing recent buffered log lines for a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLogsResponse {
    pub node_id: u64,
    pub total_buffered: usize,
    pub entries: Vec<NodeLogEntry>,
}

/// Response payload containing content and metadata of an inspected node file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfigResponse {
    pub node_id: u64,
    pub file_type: NodeConfigType,
    pub filename: String,
    #[serde(default)]
    pub path: String,
    pub exists: bool,
    pub content: String,
    pub size_bytes: u64,
    pub modified_at_ms: Option<u64>,
}
