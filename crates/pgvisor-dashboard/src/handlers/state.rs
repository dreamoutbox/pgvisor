use std::sync::Arc;
use tokio::sync::RwLock;

use pgvisor_core::audit::AuditLog;

use super::backup::{BackupService, StandaloneBackupService};
use super::cluster::{ClusterService, StandaloneClusterService};
use super::sql::{SqlExecutor, StandaloneSqlExecutor};
use super::users::{StandaloneUserService, UserService};
use crate::metrics::{MetricsService, StandaloneMetricsService};
use crate::models::{ClusterOverview, NodeHealthState, NodeRole, NodeSummary};
use crate::security::SqlSecurityGuard;

/// Shared application state for dashboard HTTP handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub overview: Arc<RwLock<ClusterOverview>>,
    pub security_guard: Arc<SqlSecurityGuard>,
    pub sql_executor: Arc<dyn SqlExecutor>,
    pub backup_service: Arc<dyn BackupService>,
    pub cluster_service: Arc<dyn ClusterService>,
    pub user_service: Arc<dyn UserService>,
    pub metrics_service: Arc<dyn MetricsService>,
    pub audit_log: Arc<AuditLog>,
    pub admin_token: Option<String>,
}

impl DashboardState {
    pub fn new(cluster_id: &str, admin_token: Option<String>) -> Self {
        let initial_overview = ClusterOverview {
            cluster_id: cluster_id.to_string(),
            current_term: 1,
            leader_id: Some(1),
            leader_address: Some("127.0.0.1:5432".to_string()),
            quorum_size: 2,
            total_nodes: 3,
            healthy_nodes: 3,
            last_backup_at: None,
            total_backups: 0,
            nodes: vec![
                NodeSummary {
                    node_id: 1,
                    address: "127.0.0.1:5432".into(),
                    role: NodeRole::Leader,
                    state: NodeHealthState::Healthy,
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 0,
                    uptime_secs: 3600,
                    is_local: true,
                },
                NodeSummary {
                    node_id: 2,
                    address: "127.0.0.1:5433".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 64,
                    uptime_secs: 3580,
                    is_local: false,
                },
                NodeSummary {
                    node_id: 3,
                    address: "127.0.0.1:5434".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 128,
                    uptime_secs: 3550,
                    is_local: false,
                },
            ],
        };

        let overview_arc = Arc::new(RwLock::new(initial_overview));
        Self {
            overview: overview_arc.clone(),
            security_guard: Arc::new(SqlSecurityGuard::default()),
            sql_executor: Arc::new(StandaloneSqlExecutor),
            backup_service: Arc::new(StandaloneBackupService::new()),
            cluster_service: Arc::new(StandaloneClusterService::new(overview_arc)),
            user_service: Arc::new(StandaloneUserService::new()),
            metrics_service: Arc::new(StandaloneMetricsService::new()),
            audit_log: Arc::new(AuditLog::new(cluster_id, None, 2000)),
            admin_token,
        }
    }
}
