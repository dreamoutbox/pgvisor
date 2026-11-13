use chrono::{DateTime, Utc};
use pgvisor_core::backup::{BasebackupMeta, RecoveryTarget};

use crate::api::ApiError;

const DEFAULT_MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;

/// Parses and normalizes a user-supplied recovery timestamp string into UTC.
///
/// # Arguments
/// * `raw` - RFC3339 or `YYYY-MM-DD HH:MM:SS` timestamp string.
///
/// # Example
/// ```rust,ignore
/// let ts = parse_target_timestamp("2026-09-14 03:00:0")?;
/// ```
pub fn parse_target_timestamp(raw: &str) -> Result<DateTime<Utc>, ApiError> {
    pgvisor_core::backup::parse_target_timestamp(raw)
        .map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// Selects the latest physical basebackup snapshot taken on or before `target_time` for forward PITR.
///
/// # Arguments
/// * `backups` - Available snapshots in object storage.
/// * `target_time` - Desired point-in-time recovery target in UTC.
///
/// # Example
/// ```rust,ignore
/// let snap = find_best_backup_snapshot(&backups, target_ts)?;
/// ```
pub fn find_best_backup_snapshot(
    backups: &[BasebackupMeta],
    target_time: DateTime<Utc>,
) -> Result<&BasebackupMeta, ApiError> {
    if backups.is_empty() {
        return Err(ApiError::BadRequest(
            "No basebackup snapshots found in storage".to_string(),
        ));
    }

    let mut eligible: Vec<&BasebackupMeta> = backups
        .iter()
        .filter(|b| b.created_at <= target_time)
        .collect();

    if eligible.is_empty() {
        if let Some(earliest) = backups.iter().min_by_key(|b| b.created_at) {
            return Err(ApiError::BadRequest(format!(
                "No basebackup snapshot found prior to target time {}. Earliest available snapshot was created at {} ({}). PostgreSQL forward recovery cannot roll backward.",
                target_time.format("%Y-%m-%d %H:%M:%S UTC"),
                earliest.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
                earliest.snapshot_id
            )));
        }
        return Err(ApiError::BadRequest(
            "No basebackup snapshots found in storage".to_string(),
        ));
    }

    eligible.sort_by_key(|b| b.created_at);
    eligible
        .last()
        .copied()
        .ok_or_else(|| ApiError::BadRequest("No eligible basebackup snapshot found".to_string()))
}

/// Returns the maximum allowed backup upload size in bytes (defaults to 1 GiB; configurable via `PGVISOR_BACKUP_MAX_UPLOAD_BYTES`).
pub fn max_upload_bytes() -> usize {
    std::env::var("PGVISOR_BACKUP_MAX_UPLOAD_BYTES")
        .or_else(|_| std::env::var("BACKUP_MAX_UPLOAD_BYTES"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES)
}

/// Validates a recovery target against snapshot metadata when present, or basic syntax rules when absent.
pub(crate) fn validate_restore_target(
    snapshot_id: &str,
    target: &RecoveryTarget,
    backups: &[BasebackupMeta],
) -> Result<(), ApiError> {
    if let Some(snap) = backups
        .iter()
        .find(|b| b.snapshot_id == snapshot_id || b.label.as_deref() == Some(snapshot_id))
    {
        pgvisor_core::backup::validate_recovery_target(target, snap, backups, Utc::now())
            .map_err(|e| ApiError::BadRequest(e.to_string()))
    } else {
        match target {
            RecoveryTarget::Time(raw) => {
                parse_target_timestamp(raw)?;
                Ok(())
            }
            RecoveryTarget::Xid(raw) => pgvisor_core::backup::validate_recovery_target_xid(
                raw,
                &BasebackupMeta::new(snapshot_id, Utc::now(), pgvisor_core::backup::BackupType::Full, "0/0", 0),
            )
            .map(|_| ())
            .map_err(|e| ApiError::BadRequest(e.to_string())),
            RecoveryTarget::Latest => Ok(()),
        }
    }
}
