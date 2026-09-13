use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

use pgvisor_core::metrics::{
    BackupMetrics, ClusterMetricsSnapshot, NodeMetricRole, NodeMetrics, ProxyMetrics,
};
use pgvisor_dashboard::handlers::{BackupService, MetricsService};
use pgvisor_dashboard::models::{ClusterOverview, NodeRole};

/// Thread-safe counters tracking query routing throughput across the proxy and backends.
#[derive(Debug, Clone, Default)]
pub struct ProxyMetricsStore {
    reads_total: Arc<AtomicU64>,
    writes_total: Arc<AtomicU64>,
    node_reads: Arc<RwLock<HashMap<u64, u64>>>,
    node_writes: Arc<RwLock<HashMap<u64, u64>>>,
}

impl ProxyMetricsStore {
    pub fn new() -> Self {
        Self {
            reads_total: Arc::new(AtomicU64::new(0)),
            writes_total: Arc::new(AtomicU64::new(0)),
            node_reads: Arc::new(RwLock::new(HashMap::new())),
            node_writes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Records a read query routed through the proxy.
    pub fn record_read(&self) {
        self.reads_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a write query routed through the proxy.
    pub fn record_write(&self) {
        self.writes_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a read query dispatched to a specific cluster node.
    pub async fn record_node_read(&self, node_id: u64) {
        self.record_read();
        let mut map = self.node_reads.write().await;
        *map.entry(node_id).or_insert(0) += 1;
    }

    /// Records a write query dispatched to a specific cluster node.
    pub async fn record_node_write(&self, node_id: u64) {
        self.record_write();
        let mut map = self.node_writes.write().await;
        *map.entry(node_id).or_insert(0) += 1;
    }

    /// Total read queries processed by the proxy.
    pub fn reads_total(&self) -> u64 {
        self.reads_total.load(Ordering::Relaxed)
    }

    /// Total write queries processed by the proxy.
    pub fn writes_total(&self) -> u64 {
        self.writes_total.load(Ordering::Relaxed)
    }

    /// Returns (reads, writes) dispatched to the specified node ID.
    pub async fn node_query_counts(&self, node_id: u64) -> (u64, u64) {
        let reads = self
            .node_reads
            .read()
            .await
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        let writes = self
            .node_writes
            .read()
            .await
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        (reads, writes)
    }
}

/// Dynamic node-level telemetry reported by sidecar status and leader replication stats.
#[derive(Clone, Debug, Default)]
pub struct NodeTelemetry {
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub replication_lag_bytes: u64,
    pub uptime_secs: u64,
}

/// Production implementation of MetricsService aggregating live telemetry across nodes and proxy.
#[derive(Clone)]
pub struct ProxyMetricsService {
    store: Arc<ProxyMetricsStore>,
    overview: Arc<RwLock<ClusterOverview>>,
    telemetry: Arc<RwLock<HashMap<u64, NodeTelemetry>>>,
    backup_service: Arc<dyn BackupService>,
    history: Arc<RwLock<VecDeque<ClusterMetricsSnapshot>>>,
}

impl ProxyMetricsService {
    pub fn new(
        store: Arc<ProxyMetricsStore>,
        overview: Arc<RwLock<ClusterOverview>>,
        telemetry: Arc<RwLock<HashMap<u64, NodeTelemetry>>>,
        backup_service: Arc<dyn BackupService>,
        history_capacity: usize,
    ) -> Self {
        Self {
            store,
            overview,
            telemetry,
            backup_service,
            history: Arc::new(RwLock::new(VecDeque::with_capacity(history_capacity))),
        }
    }

    /// Updates dynamic telemetry for a given node.
    pub async fn update_node_telemetry(&self, node_id: u64, data: NodeTelemetry) {
        let mut map = self.telemetry.write().await;
        map.insert(node_id, data);
    }

    /// Records the current snapshot into the rolling history ring buffer.
    pub async fn record_tick(&self) -> ClusterMetricsSnapshot {
        let snap = self.current_snapshot().await;
        let mut hist = self.history.write().await;
        if hist.len() >= 60 {
            hist.pop_front();
        }
        hist.push_back(snap.clone());
        snap
    }
}

#[async_trait::async_trait]
impl MetricsService for ProxyMetricsService {
    async fn current_snapshot(&self) -> ClusterMetricsSnapshot {
        let timestamp_ms = chrono::Utc::now().timestamp_millis() as u64;

        let overview_nodes = {
            let ov = self.overview.read().await;
            ov.nodes.clone()
        };

        let telemetry_map = {
            let t = self.telemetry.read().await;
            t.clone()
        };

        let mut nodes = Vec::with_capacity(overview_nodes.len());
        for n in overview_nodes {
            let telem = telemetry_map.get(&n.node_id).cloned().unwrap_or_default();
            let (reads, writes) = self.store.node_query_counts(n.node_id).await;

            let role = match n.role {
                NodeRole::Leader => NodeMetricRole::Leader,
                NodeRole::Standby => NodeMetricRole::Standby,
                NodeRole::Learner => NodeMetricRole::Learner,
            };

            let uptime = if telem.uptime_secs > 0 {
                telem.uptime_secs
            } else {
                n.uptime_secs
            };

            let lag = if n.role == NodeRole::Leader {
                0
            } else if telem.replication_lag_bytes > 0 {
                telem.replication_lag_bytes
            } else {
                n.replication_lag_bytes
            };

            nodes.push(NodeMetrics {
                node_id: n.node_id,
                address: n.address,
                role,
                uptime_secs: uptime,
                replication_lag_bytes: lag,
                cpu_percent: telem.cpu_percent,
                memory_used_bytes: telem.memory_used_bytes,
                memory_total_bytes: telem.memory_total_bytes,
                pg_reads: reads,
                pg_writes: writes,
            });
        }

        let proxy = ProxyMetrics {
            reads_total: self.store.reads_total(),
            writes_total: self.store.writes_total(),
        };

        let backups = match self.backup_service.list_backups().await {
            Ok(list) => {
                let count = list.len();
                let total_size: u64 = list.iter().map(|b| b.total_bytes).sum();
                let throughput = if count > 0 {
                    let avg = total_size as f64 / count as f64;
                    avg.min(50_000_000.0)
                } else {
                    0.0
                };
                BackupMetrics {
                    total_backups: count,
                    total_size_bytes: total_size,
                    throughput_bytes_per_sec: throughput,
                    success_count: count as u64,
                    fail_count: 0,
                }
            }
            Err(_) => BackupMetrics::default(),
        };

        ClusterMetricsSnapshot {
            timestamp_ms,
            nodes,
            proxy,
            backups,
        }
    }

    async fn history(&self) -> Vec<ClusterMetricsSnapshot> {
        let hist = self.history.read().await;
        hist.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_proxy_metrics_store_counters() {
        let store = ProxyMetricsStore::new();
        assert_eq!(store.reads_total(), 0);
        assert_eq!(store.writes_total(), 0);

        store.record_read();
        store.record_write();
        store.record_write();

        assert_eq!(store.reads_total(), 1);
        assert_eq!(store.writes_total(), 2);

        store.record_node_read(2).await;
        store.record_node_write(1).await;

        assert_eq!(store.reads_total(), 2);
        assert_eq!(store.writes_total(), 3);

        let (r1, w1) = store.node_query_counts(1).await;
        assert_eq!(r1, 0);
        assert_eq!(w1, 1);

        let (r2, w2) = store.node_query_counts(2).await;
        assert_eq!(r2, 1);
        assert_eq!(w2, 0);
    }
}
