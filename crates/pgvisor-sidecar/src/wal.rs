use std::env;

use anyhow::Result;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig};

/// Executes WAL segment archiving command to remote S3/MinIO storage.
pub async fn run_archive_wal(source_path: &str, file_name: &str) -> Result<()> {
    let s3_endpoint = env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
    let s3_bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "pgvisor-backups".to_string());
    let s3_access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let s3_secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let cluster_id =
        env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());

    let backup_config = BackupScheduleConfig {
        minio_endpoint: s3_endpoint,
        minio_bucket: s3_bucket,
        access_key: s3_access_key,
        secret_key: s3_secret_key,
        ..Default::default()
    };

    let operator = backup_config.build_operator().map_err(|e| {
        eprintln!("Failed to build OpenDAL operator for WAL archive: {e}");
        anyhow::anyhow!("{e}")
    })?;

    let manager = BackupManager::new(&cluster_id, operator);
    manager
        .archive_wal(source_path, file_name)
        .await
        .map_err(|e| {
            eprintln!("Failed to archive WAL segment {file_name}: {e}");
            anyhow::anyhow!("{e}")
        })?;

    Ok(())
}

/// Executes WAL segment restore command from remote S3/MinIO storage.
pub async fn run_restore_wal(file_name: &str, target_path: &str) -> Result<()> {
    let s3_endpoint = env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
    let s3_bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "pgvisor-backups".to_string());
    let s3_access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let s3_secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let cluster_id =
        env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());

    let backup_config = BackupScheduleConfig {
        minio_endpoint: s3_endpoint,
        minio_bucket: s3_bucket,
        access_key: s3_access_key,
        secret_key: s3_secret_key,
        ..Default::default()
    };

    let operator = backup_config.build_operator().map_err(|e| {
        eprintln!("Failed to build OpenDAL operator for WAL restore: {e}");
        anyhow::anyhow!("{e}")
    })?;

    let manager = BackupManager::new(&cluster_id, operator);
    match manager.restore_wal(file_name, target_path).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            // Non-zero exit code informs PostgreSQL that the requested WAL segment was not found in archive
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to restore WAL segment {file_name}: {e}");
            std::process::exit(1);
        }
    }
}
