use std::sync::Arc;

use super::sidecar_client::SidecarClient;

/// Shared state for the standalone UI-only PgVisor dashboard service.
#[derive(Clone)]
pub struct ClientDashboardState {
    pub sidecar_client: Arc<SidecarClient>,
    pub cluster_id: String,
    pub storage_endpoint: String,
    pub storage_bucket: String,
    pub retention_days: u32,
    pub keep_count: Option<usize>,
}

impl ClientDashboardState {
    /// Creates [`ClientDashboardState`] connected to `sidecar_client`.
    pub fn new(sidecar_client: Arc<SidecarClient>, cluster_id: impl Into<String>) -> Self {
        let storage_endpoint = std::env::var("S3_ENDPOINT")
            .or_else(|_| std::env::var("PGVISOR_S3_ENDPOINT"))
            .unwrap_or_else(|_| "http://minio:9000".to_string());
        let storage_bucket = std::env::var("S3_BUCKET")
            .or_else(|_| std::env::var("PGVISOR_S3_BUCKET"))
            .unwrap_or_else(|_| "pgvisor-backups".to_string());
        let retention_days = std::env::var("PGVISOR_BACKUP_RETENTION_DAYS")
            .or_else(|_| std::env::var("BACKUP_RETENTION_DAYS"))
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(7);
        let keep_count = std::env::var("PGVISOR_BACKUP_KEEP_COUNT")
            .or_else(|_| std::env::var("BACKUP_KEEP_COUNT"))
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map_or(Some(10), |n| if n == 0 { None } else { Some(n) });

        Self {
            sidecar_client,
            cluster_id: cluster_id.into(),
            storage_endpoint,
            storage_bucket,
            retention_days,
            keep_count,
        }
    }
}
