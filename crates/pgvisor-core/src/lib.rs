pub mod audit;
pub mod auth;
pub mod backup;
pub mod error;
pub mod logging;
pub mod metrics;
pub mod node;
pub mod protocol;
pub mod raft;
pub mod storage;
pub mod tls;

pub use audit::{AuditError, AuditEvent, AuditEventKind, AuditLog};
pub use auth::{
    cluster_secret_from_env, derive_bearer_token, make_auth_header_value, validate_bearer_token,
    CLUSTER_SECRET_ENV,
};
pub use backup::{BackupError, BackupManager, BasebackupMeta};
pub use error::StorageError;
pub use logging::{
    extract_node_name, format_backup_highlight, format_become_leader_highlight,
    format_highlight_banner, format_leader_down_highlight, format_restore_highlight, log_highlight,
};
pub use metrics::{
    BackupMetrics, ClusterMetricsSnapshot, NodeMetricRole, NodeMetrics, ProxyMetrics,
};
pub use node::{LogLevel, NodeConfigResponse, NodeConfigType, NodeLogEntry, NodeLogsResponse};
pub use protocol::{
    BackendMessage, FrontendMessage, InitialClientMessage, QueryKind, StartupMessage,
    TransactionStatus, TransactionTracker,
};
pub use raft::{
    ClusterCommand, ClusterResponse, FailoverOrchestrator, NodeInfo, NodeRole, OrchestratorAction,
    QuorumLease, TypeConfig,
};
pub use storage::{LogReader, LogStore, SnapshotBuilder, StateMachine, StateMachineStore, Wal};
