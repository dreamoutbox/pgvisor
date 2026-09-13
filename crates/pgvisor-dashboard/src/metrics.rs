use std::collections::VecDeque;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use tokio::sync::RwLock;

use crate::handlers::DashboardState;
use crate::models::{
    BackupMetrics, ClusterMetricsSnapshot, NodeMetricRole, NodeMetrics, ProxyMetrics,
};

/// Abstraction for collecting cluster performance and health metrics.
#[async_trait::async_trait]
pub trait MetricsService: Send + Sync {
    /// Returns the most recent point-in-time metrics snapshot.
    async fn current_snapshot(&self) -> ClusterMetricsSnapshot;

    /// Returns the rolling historical sequence of snapshots.
    async fn history(&self) -> Vec<ClusterMetricsSnapshot>;
}

/// In-memory standalone metrics service providing simulated metrics for local development and testing.
pub struct StandaloneMetricsService {
    history: Arc<RwLock<VecDeque<ClusterMetricsSnapshot>>>,
}

impl Default for StandaloneMetricsService {
    fn default() -> Self {
        Self::new()
    }
}

impl StandaloneMetricsService {
    pub fn new() -> Self {
        let mut history = VecDeque::new();
        let now = chrono::Utc::now().timestamp_millis() as u64;

        // Populate baseline history points (60 points spaced 5s apart = 5min window)
        for i in (0..60).rev() {
            let ts = now.saturating_sub(i * 5000);
            let factor = (60 - i) as u64;
            history.push_back(ClusterMetricsSnapshot {
                timestamp_ms: ts,
                nodes: vec![
                    NodeMetrics {
                        node_id: 1,
                        address: "127.0.0.1:5432".to_string(),
                        role: NodeMetricRole::Leader,
                        uptime_secs: 3600 + factor * 5,
                        replication_lag_bytes: 0,
                        cpu_percent: 12.0 + ((i % 7) as f32 * 1.5),
                        memory_used_bytes: 250_000_000 + factor * 500_000,
                        memory_total_bytes: 1_073_741_824,
                        pg_reads: 1200 + factor * 12,
                        pg_writes: 450 + factor * 6,
                    },
                    NodeMetrics {
                        node_id: 2,
                        address: "127.0.0.1:5433".to_string(),
                        role: NodeMetricRole::Standby,
                        uptime_secs: 3580 + factor * 5,
                        replication_lag_bytes: 64 + (i % 5) * 16,
                        cpu_percent: 8.0 + ((i % 4) as f32 * 1.2),
                        memory_used_bytes: 210_000_000 + factor * 300_000,
                        memory_total_bytes: 1_073_741_824,
                        pg_reads: 850 + factor * 8,
                        pg_writes: 0,
                    },
                    NodeMetrics {
                        node_id: 3,
                        address: "127.0.0.1:5434".to_string(),
                        role: NodeMetricRole::Standby,
                        uptime_secs: 3550 + factor * 5,
                        replication_lag_bytes: 128 + (i % 6) * 32,
                        cpu_percent: 7.5 + ((i % 3) as f32 * 1.1),
                        memory_used_bytes: 195_000_000 + factor * 250_000,
                        memory_total_bytes: 1_073_741_824,
                        pg_reads: 780 + factor * 7,
                        pg_writes: 0,
                    },
                ],
                proxy: ProxyMetrics {
                    reads_total: 1630 + factor * 15,
                    writes_total: 450 + factor * 6,
                },
                backups: BackupMetrics {
                    total_backups: 2,
                    total_size_bytes: 17_300_000,
                    throughput_bytes_per_sec: 1_450_000.0,
                    success_count: 5,
                    fail_count: 0,
                },
            });
        }

        Self {
            history: Arc::new(RwLock::new(history)),
        }
    }
}

#[async_trait::async_trait]
impl MetricsService for StandaloneMetricsService {
    async fn current_snapshot(&self) -> ClusterMetricsSnapshot {
        let history = self.history.read().await;
        if let Some(last) = history.back() {
            let mut snap = last.clone();
            snap.timestamp_ms = chrono::Utc::now().timestamp_millis() as u64;
            snap
        } else {
            ClusterMetricsSnapshot {
                timestamp_ms: chrono::Utc::now().timestamp_millis() as u64,
                nodes: Vec::new(),
                proxy: ProxyMetrics::default(),
                backups: BackupMetrics::default(),
            }
        }
    }

    async fn history(&self) -> Vec<ClusterMetricsSnapshot> {
        let history = self.history.read().await;
        history.iter().cloned().collect()
    }
}

/// GET /api/metrics/snapshot -> Returns current point-in-time metrics snapshot
pub async fn api_metrics_snapshot(
    State(state): State<Arc<DashboardState>>,
) -> Json<ClusterMetricsSnapshot> {
    let snap = state.metrics_service.current_snapshot().await;
    Json(snap)
}

/// GET /api/metrics/history -> Returns rolling time-series history of metrics snapshots
pub async fn api_metrics_history(
    State(state): State<Arc<DashboardState>>,
) -> Json<Vec<ClusterMetricsSnapshot>> {
    let hist = state.metrics_service.history().await;
    Json(hist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_standalone_metrics_service_snapshot_and_history() {
        let service = StandaloneMetricsService::new();
        let snapshot = service.current_snapshot().await;
        assert_eq!(snapshot.nodes.len(), 3);
        assert_eq!(snapshot.proxy.writes_total, 450 + 60 * 6);

        let hist = service.history().await;
        assert_eq!(hist.len(), 60);
    }
}
