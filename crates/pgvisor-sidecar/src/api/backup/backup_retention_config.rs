use pgvisor_core::backup::BackupScheduleConfig;

/// Storage and retention metadata exposed alongside backup listings and applied on pruning.
#[derive(Clone, Debug)]
pub struct BackupRetentionConfig {
    pub endpoint: String,
    pub bucket: String,
    pub retention_days: u32,
    pub keep_count: Option<usize>,
}

impl BackupRetentionConfig {
    /// Builds retention settings from a [`BackupScheduleConfig`].
    pub fn from_schedule_config(cfg: &BackupScheduleConfig) -> Self {
        Self {
            endpoint: cfg.minio_endpoint.clone(),
            bucket: cfg.minio_bucket.clone(),
            retention_days: cfg.retention_days,
            keep_count: cfg.keep_count,
        }
    }
}

impl Default for BackupRetentionConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:9000".to_string(),
            bucket: "pgvisor-backups".to_string(),
            retention_days: 7,
            keep_count: Some(10),
        }
    }
}
