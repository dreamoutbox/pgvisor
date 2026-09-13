use serde::{Deserialize, Serialize};

/// High-level operational role for metric reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeMetricRole {
    Leader,
    Standby,
    Learner,
    Unknown,
}

impl std::fmt::Display for NodeMetricRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Leader => write!(f, "Leader"),
            Self::Standby => write!(f, "Standby"),
            Self::Learner => write!(f, "Learner"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Snapshot metrics for an individual PostgreSQL cluster node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeMetrics {
    pub node_id: u64,
    pub address: String,
    pub role: NodeMetricRole,
    pub uptime_secs: u64,
    pub replication_lag_bytes: u64,
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub pg_reads: u64,
    pub pg_writes: u64,
}

/// Cumulative query routing metrics from the L7 connection proxy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProxyMetrics {
    pub reads_total: u64,
    pub writes_total: u64,
}

/// Aggregated backup metrics including size, throughput, and success rate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct BackupMetrics {
    pub total_backups: usize,
    pub total_size_bytes: u64,
    pub throughput_bytes_per_sec: f64,
    pub success_count: u64,
    pub fail_count: u64,
}

/// Point-in-time snapshot of all cluster performance and health metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClusterMetricsSnapshot {
    pub timestamp_ms: u64,
    pub nodes: Vec<NodeMetrics>,
    pub proxy: ProxyMetrics,
    pub backups: BackupMetrics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_serialization_roundtrip() {
        let snapshot = ClusterMetricsSnapshot {
            timestamp_ms: 1726272000000,
            nodes: vec![NodeMetrics {
                node_id: 1,
                address: "127.0.0.1:5432".to_string(),
                role: NodeMetricRole::Leader,
                uptime_secs: 3600,
                replication_lag_bytes: 0,
                cpu_percent: 12.5,
                memory_used_bytes: 104857600,
                memory_total_bytes: 1073741824,
                pg_reads: 1200,
                pg_writes: 450,
            }],
            proxy: ProxyMetrics {
                reads_total: 500,
                writes_total: 250,
            },
            backups: BackupMetrics {
                total_backups: 3,
                total_size_bytes: 52428800,
                throughput_bytes_per_sec: 1048576.0,
                success_count: 3,
                fail_count: 0,
            },
        };

        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let deserialized: ClusterMetricsSnapshot =
            serde_json::from_str(&json).expect("deserialize snapshot");
        assert_eq!(snapshot, deserialized);
    }
}
