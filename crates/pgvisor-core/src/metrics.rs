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
    #[serde(default)]
    pub disk_used_bytes: u64,
    pub pg_reads: u64,
    pub pg_writes: u64,
}

/// Cumulative cluster activity aggregated from per-node `pg_stat_database` counters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ClusterActivity {
    pub reads_total: u64,
    pub writes_total: u64,
}

/// Per-node PostgreSQL activity counters sampled from `pg_stat_database`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PgNodeActivity {
    pub xact_commit: u64,
    pub xact_rollback: u64,
    pub tup_returned: u64,
    pub tup_fetched: u64,
    pub tup_inserted: u64,
    pub tup_updated: u64,
    pub tup_deleted: u64,
}

impl PgNodeActivity {
    /// Total tuple read operations (`tup_returned + tup_fetched`).
    pub fn reads(&self) -> u64 {
        self.tup_returned.saturating_add(self.tup_fetched)
    }

    /// Total tuple write operations (`tup_inserted + tup_updated + tup_deleted`).
    pub fn writes(&self) -> u64 {
        self.tup_inserted
            .saturating_add(self.tup_updated)
            .saturating_add(self.tup_deleted)
    }
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
    pub activity: ClusterActivity,
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
                disk_used_bytes: 52428800,
                pg_reads: 1200,
                pg_writes: 450,
            }],
            activity: ClusterActivity {
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
