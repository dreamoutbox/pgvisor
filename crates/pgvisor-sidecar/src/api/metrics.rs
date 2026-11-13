use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use pgvisor_core::api::{
    BackupMetrics, ClusterMetricsSnapshot, NodeMetricRole, NodeMetrics, SidecarRole,
};
use pgvisor_core::pg_client::PgConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use super::api_state::ApiState;
use super::peer_probe::{probe_all_peers, PeerProbe};
use super::pg_activity::{sum_cluster_activity, ActivityTracker};

const METRICS_RING_CAPACITY: usize = 60;
const METRICS_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const LAG_QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Rolling 60-sample metrics history ring paired with monotonic `ActivityTracker`.
#[derive(Debug)]
pub struct MetricsRing {
    history: RwLock<VecDeque<ClusterMetricsSnapshot>>,
    activity_tracker: Mutex<ActivityTracker>,
}

impl MetricsRing {
    /// Initializes an empty metrics ring of capacity 60.
    pub fn new() -> Self {
        Self {
            history: RwLock::new(VecDeque::with_capacity(METRICS_RING_CAPACITY)),
            activity_tracker: Mutex::new(ActivityTracker::default()),
        }
    }

    /// Appends a snapshot to the ring buffer, evicting the oldest entry past 60 samples.
    pub async fn push(&self, snapshot: ClusterMetricsSnapshot) {
        let mut hist = self.history.write().await;
        if hist.len() >= METRICS_RING_CAPACITY {
            hist.pop_front();
        }
        hist.push_back(snapshot);
    }

    /// Returns all buffered historical snapshots in chronological order.
    pub async fn snapshot_history(&self) -> Vec<ClusterMetricsSnapshot> {
        self.history.read().await.iter().cloned().collect()
    }

    /// Updates monotonic per-node `(reads, writes)` from a fresh `StatusResponse` activity reading.
    pub async fn observe_node(
        &self,
        node_id: u64,
        activity: Option<pgvisor_core::metrics::PgNodeActivity>,
    ) -> (u64, u64) {
        self.activity_tracker
            .lock()
            .await
            .observe(node_id, activity)
    }
}

impl Default for MetricsRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Queries replication lag (in bytes) per replica from `pg_stat_replication` on a leader Postgres.
pub(crate) async fn query_replication_lags(
    pg_addr: &str,
    pg_superuser: &str,
) -> HashMap<String, u64> {
    let mut lags = HashMap::new();
    let sql = "SELECT application_name, COALESCE(client_addr::text, ''), COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)::bigint, 0) FROM pg_stat_replication;";
    let Ok(Ok(res)) = tokio::time::timeout(
        LAG_QUERY_TIMEOUT,
        PgConnection::query_simple(pg_addr, Some(pg_superuser), Some("postgres"), sql, 64),
    )
    .await
    else {
        return lags;
    };

    for row in res.rows {
        let app_name = row.first().map(|s| s.trim()).unwrap_or("");
        let client_addr = row.get(1).map(|s| s.trim()).unwrap_or("");
        let lag = row
            .get(2)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if !app_name.is_empty() {
            lags.insert(app_name.to_string(), lag);
        }
        if !client_addr.is_empty() {
            lags.insert(client_addr.to_string(), lag);
        }
    }
    lags
}

pub(crate) fn lookup_lag_for_peer(
    lags: &HashMap<String, u64>,
    node_id: u64,
    pg_address: &str,
) -> u64 {
    let app_key = format!("pgvisor-node{node_id}");
    let host = pg_address.split(':').next().unwrap_or(pg_address);
    lags.get(&app_key)
        .or_else(|| lags.get(host))
        .or_else(|| lags.get(pg_address))
        .copied()
        .unwrap_or(0)
}

async fn map_probe_to_node_metrics(
    probe: &PeerProbe,
    tracker: &MetricsRing,
    replica_lags: &HashMap<String, u64>,
) -> Option<NodeMetrics> {
    let status = probe.status.as_ref()?;
    let role_enum = SidecarRole::from_role_str(&status.role);
    let metric_role = match role_enum {
        SidecarRole::Leader => NodeMetricRole::Leader,
        SidecarRole::Standby => NodeMetricRole::Standby,
        SidecarRole::Fenced => NodeMetricRole::Unknown,
    };

    let (pg_reads, pg_writes) = tracker.observe_node(status.node_id, status.activity).await;
    let replication_lag_bytes = if role_enum == SidecarRole::Leader {
        0
    } else {
        lookup_lag_for_peer(replica_lags, status.node_id, &probe.pg_address)
    };

    Some(NodeMetrics {
        node_id: status.node_id,
        address: probe.pg_address.clone(),
        role: metric_role,
        uptime_secs: status.uptime_secs,
        replication_lag_bytes,
        cpu_percent: status.cpu_percent,
        memory_used_bytes: status.memory_used_bytes,
        memory_total_bytes: status.memory_total_bytes,
        disk_used_bytes: status.disk_used_bytes,
        pg_reads,
        pg_writes,
    })
}

/// Builds a fresh [`ClusterMetricsSnapshot`] from peer `/control/status` and S3 backups.
///
/// # Arguments
/// * `state` - Shared API state.
pub async fn build_metrics_snapshot(state: &ApiState) -> ClusterMetricsSnapshot {
    let timestamp_ms = chrono::Utc::now().timestamp_millis() as u64;
    let probes = probe_all_peers(state).await;

    let replica_lags = if state.sidecar.current_role().await == SidecarRole::Leader {
        query_replication_lags(&state.pg_addr, &state.pg_superuser).await
    } else {
        HashMap::new()
    };

    let mut nodes = Vec::with_capacity(probes.len());
    for probe in &probes {
        if let Some(node_metrics) =
            map_probe_to_node_metrics(probe, &state.metrics_ring, &replica_lags).await
        {
            nodes.push(node_metrics);
        }
    }

    let activity = sum_cluster_activity(&nodes);
    let backups = collect_backup_metrics(state).await;

    ClusterMetricsSnapshot {
        timestamp_ms,
        nodes,
        activity,
        backups,
    }
}

async fn collect_backup_metrics(state: &ApiState) -> BackupMetrics {
    let Some(ref manager) = state.sidecar.backup_manager else {
        return BackupMetrics::default();
    };
    let Ok(list) = manager.list_basebackups().await else {
        return BackupMetrics::default();
    };
    let total_backups = list.len();
    let total_size_bytes: u64 = list.iter().map(|b| b.total_bytes).sum();
    let throughput_bytes_per_sec = if total_backups > 0 {
        let avg = total_size_bytes as f64 / total_backups as f64;
        avg.min(50_000_000.0)
    } else {
        0.0
    };
    BackupMetrics {
        total_backups,
        total_size_bytes,
        throughput_bytes_per_sec,
        success_count: total_backups as u64,
        fail_count: 0,
    }
}

/// Spawns the 5-second background sampler pushing snapshots into `state.metrics_ring`.
pub fn spawn_metrics_sampler(state: ApiState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(METRICS_SAMPLE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let snapshot = build_metrics_snapshot(&state).await;
            debug!(
                nodes = snapshot.nodes.len(),
                reads_total = snapshot.activity.reads_total,
                writes_total = snapshot.activity.writes_total,
                "Recorded cluster metrics snapshot"
            );
            state.metrics_ring.push(snapshot).await;
        }
    });
}

/// Handler for `GET /api/metrics/snapshot`.
pub async fn get_metrics_snapshot(State(state): State<ApiState>) -> Json<ClusterMetricsSnapshot> {
    let snapshot = build_metrics_snapshot(&state).await;
    Json(snapshot)
}

/// Handler for `GET /api/metrics/history`.
pub async fn get_metrics_history(
    State(state): State<ApiState>,
) -> Json<Vec<ClusterMetricsSnapshot>> {
    let history = state.metrics_ring.snapshot_history().await;
    Json(history)
}
