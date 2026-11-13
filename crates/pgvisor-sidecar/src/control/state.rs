use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use pgvisor_core::api::SidecarRole;
use pgvisor_core::backup::BackupManager;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::PostgresConfig;
use crate::supervisor::PostgresSupervisor;
use crate::system::SystemMetricsCollector;

pub const DEFAULT_API_PORT: u16 = 8081;

fn default_api_port() -> u16 {
    DEFAULT_API_PORT
}

/// Auditable event record tracked within local sidecar lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarEventRecord {
    pub id: u64,
    pub timestamp: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone)]
pub struct SidecarState {
    pub supervisor: Arc<PostgresSupervisor>,
    pub config: Arc<RwLock<PostgresConfig>>,
    pub backup_manager: Option<Arc<BackupManager>>,
    pub node_id: u64,
    pub role: Arc<RwLock<String>>,
    pub pg_version: String,
    pub events: Arc<RwLock<VecDeque<SidecarEventRecord>>>,
    pub event_id: Arc<AtomicU64>,
    pub system_metrics: Arc<SystemMetricsCollector>,
    pub peers: Arc<Vec<String>>,
    pub cluster_secret: Option<String>,
    /// Cluster-wide backup/restore lock state on the leader with auto-expiry.
    pub backup_lock: Arc<RwLock<Option<BackupLockInfo>>>,
    /// HTTP port serving the client-facing `/api/*` router.
    pub api_port: u16,
}

impl SidecarState {
    pub fn new(
        supervisor: Arc<PostgresSupervisor>,
        config: Arc<RwLock<PostgresConfig>>,
        backup_manager: Option<Arc<BackupManager>>,
        node_id: u64,
        role: Arc<RwLock<String>>,
        pg_version: String,
        peers: Arc<Vec<String>>,
        cluster_secret: Option<String>,
    ) -> Self {
        Self {
            supervisor,
            config,
            backup_manager,
            node_id,
            role,
            pg_version,
            events: Arc::new(RwLock::new(VecDeque::new())),
            event_id: Arc::new(AtomicU64::new(1)),
            system_metrics: Arc::new(SystemMetricsCollector::new()),
            peers,
            cluster_secret,
            backup_lock: Arc::new(RwLock::new(None)),
            api_port: DEFAULT_API_PORT,
        }
    }

    /// Sets the sidecar client API port reported in status responses.
    pub fn with_api_port(mut self, api_port: u16) -> Self {
        self.api_port = api_port;
        self
    }

    /// Returns the typed [`SidecarRole`] of this node.
    pub async fn current_role(&self) -> SidecarRole {
        let raw = self.role.read().await;
        SidecarRole::from_role_str(&raw)
    }

    pub async fn record_event(&self, kind: &str, detail: impl Into<String>) {
        let id = self.event_id.fetch_add(1, Ordering::SeqCst);
        let record = SidecarEventRecord {
            id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind: kind.to_string(),
            detail: detail.into(),
        };
        let mut q = self.events.write().await;
        if q.len() >= 200 {
            q.pop_front();
        }
        q.push_back(record);
    }
}

#[derive(Deserialize)]
pub struct RestorePayload {
    pub snapshot_id: String,
    pub recovery_target_time: Option<String>,
    pub recovery_target_xid: Option<String>,
    #[serde(default)]
    pub recovery_target: Option<pgvisor_core::backup::RecoveryTarget>,
}

impl RestorePayload {
    /// Resolves the intended recovery target specification, giving precedence to explicit
    /// enum values followed by XID and time strings.
    pub fn target(&self) -> Option<pgvisor_core::backup::RecoveryTarget> {
        if let Some(ref target) = self.recovery_target {
            Some(target.clone())
        } else if let Some(ref xid) = self.recovery_target_xid {
            let trimmed = xid.trim();
            if !trimmed.is_empty() {
                Some(pgvisor_core::backup::RecoveryTarget::Xid(trimmed.to_string()))
            } else {
                None
            }
        } else if let Some(ref time) = self.recovery_target_time {
            let trimmed = time.trim();
            if !trimmed.is_empty() {
                Some(pgvisor_core::backup::RecoveryTarget::Time(trimmed.to_string()))
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[derive(Deserialize, Default)]
pub struct ResyncPayload {
    pub primary_conninfo: Option<String>,
}

#[derive(Deserialize)]
pub struct RepointPayload {
    pub primary_conninfo: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct StatusResponse {
    pub node_id: u64,
    pub role: String,
    pub status: String,
    pub child_pid: u32,
    pub pg_version: String,
    pub uptime_secs: u64,
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    #[serde(default)]
    pub disk_used_bytes: u64,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
}

#[derive(Deserialize)]
pub struct EventsQuery {
    pub since_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub limit: Option<usize>,
}

/// Tracks cluster-wide backup/restore lock state on the leader.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupLockInfo {
    pub token: String,
    pub acquired_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Response returned when a backup lock is successfully acquired.
#[derive(Serialize)]
pub struct AcquireBackupLockResponse {
    pub lock_token: String,
    pub acquired_at: String,
    pub expires_at: String,
}

/// Payload required to release a backup lock.
#[derive(Deserialize)]
pub struct ReleaseLockPayload {
    pub lock_token: String,
}
