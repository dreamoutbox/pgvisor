use serde::{Deserialize, Serialize};

use super::format::format_bytes;
use super::node_health_state::NodeHealthState;
use super::node_role::NodeRole;

/// Snapshot metrics and status of a cluster node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: u64,
    pub address: String,
    pub role: NodeRole,
    pub state: NodeHealthState,
    pub pg_version: String,
    pub replication_lag_bytes: u64,
    pub uptime_secs: u64,
    pub is_local: bool,
    #[serde(default)]
    pub cpu_percent: f32,
    #[serde(default)]
    pub memory_used_bytes: u64,
    #[serde(default)]
    pub memory_total_bytes: u64,
    #[serde(default)]
    pub disk_used_bytes: u64,
}

impl NodeSummary {
    pub fn cpu_pretty(&self) -> String {
        format!("{:.1}%", self.cpu_percent)
    }

    pub fn memory_pretty(&self) -> String {
        if self.memory_total_bytes > 0 {
            format!(
                "{} / {}",
                format_bytes(self.memory_used_bytes),
                format_bytes(self.memory_total_bytes)
            )
        } else {
            "-".to_string()
        }
    }

    pub fn memory_percent(&self) -> f32 {
        if self.memory_total_bytes > 0 {
            ((self.memory_used_bytes as f64 / self.memory_total_bytes as f64) * 100.0) as f32
        } else {
            0.0
        }
    }

    pub fn disk_pretty(&self) -> String {
        if self.disk_used_bytes > 0 {
            format_bytes(self.disk_used_bytes)
        } else {
            "-".to_string()
        }
    }
}
