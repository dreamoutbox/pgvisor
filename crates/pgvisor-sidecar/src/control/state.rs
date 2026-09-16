use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use pgvisor_core::backup::BackupManager;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::PostgresConfig;
use crate::supervisor::PostgresSupervisor;
use crate::system::SystemMetricsCollector;

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
        }
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
}

#[derive(Deserialize)]
pub struct EventsQuery {
    pub since_id: Option<u64>,
}
