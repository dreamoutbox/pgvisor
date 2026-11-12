use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::Json;
use chrono::Utc;
use pgvisor_core::backup::{generate_snapshot_id, BackupType, BasebackupMeta};
use tokio::sync::RwLock;
use tracing::error;

use super::state::DashboardState;
use crate::models::{
    format_bytes, BackupItemView, BackupOverviewSummary, BestBackupQuery, BestBackupResponse,
    CreateBackupRequest, QuickRestoreRequest, QuickRestoreResponse, RestoreBackupRequest,
};
use crate::templates::BackupsTemplate;

/// Helper to parse a target recovery timestamp flexibly.
///
/// Delegates parsing and format normalization to [`pgvisor_core::backup::parse_target_timestamp`].
pub fn parse_target_timestamp(raw: &str) -> Result<chrono::DateTime<Utc>, String> {
    pgvisor_core::backup::parse_target_timestamp(raw).map_err(|e| e.to_string())
}

/// Helper to select the best basebackup snapshot for forward Point-In-Time-Recovery.
/// In PostgreSQL PITR, physical basebackups cannot roll backward: only snapshots taken
/// ON OR BEFORE the target timestamp can replay WAL forward to reach the target.
pub fn find_best_backup_snapshot<'a>(
    backups: &'a [BasebackupMeta],
    target_time: chrono::DateTime<Utc>,
) -> Result<&'a BasebackupMeta, String> {
    if backups.is_empty() {
        return Err("No basebackup snapshots found in storage".to_string());
    }

    let mut eligible: Vec<&BasebackupMeta> = backups
        .iter()
        .filter(|b| b.created_at <= target_time)
        .collect();

    if eligible.is_empty() {
        let earliest = backups.iter().min_by_key(|b| b.created_at).unwrap();
        return Err(format!(
            "No basebackup snapshot found prior to target time {}. Earliest available snapshot was created at {} ({}). PostgreSQL forward recovery cannot roll backward.",
            target_time.format("%Y-%m-%d %H:%M:%S UTC"),
            earliest.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            earliest.snapshot_id
        ));
    }

    // Sort ascending, pick the latest snapshot before or at target time
    eligible.sort_by_key(|b| b.created_at);
    Ok(eligible.last().unwrap())
}

/// Abstraction for backup and restore operations across physical storage and Postgres nodes.
#[async_trait::async_trait]
pub trait BackupService: Send + Sync {
    async fn list_backups(&self) -> Result<Vec<BasebackupMeta>, String>;
    async fn create_backup(
        &self,
        backup_type: BackupType,
        label: Option<String>,
    ) -> Result<BasebackupMeta, String>;
    async fn restore_backup(
        &self,
        snapshot_id: &str,
        target_time: Option<String>,
    ) -> Result<String, String>;
    async fn quick_restore(&self, target_time: &str) -> Result<(BasebackupMeta, String), String> {
        let parsed_target = parse_target_timestamp(target_time)?;
        let backups = self.list_backups().await?;
        let best = find_best_backup_snapshot(&backups, parsed_target)?.clone();
        let formatted_target = parsed_target.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        let msg = self
            .restore_backup(&best.snapshot_id, Some(formatted_target))
            .await?;
        Ok((best, msg))
    }
    async fn delete_backup(&self, snapshot_id: &str) -> Result<(), String>;
    async fn get_backup_archive(
        &self,
        snapshot_id: &str,
    ) -> Result<(BasebackupMeta, Vec<u8>), String>;
    fn storage_info(&self) -> (String, String, u32, Option<usize>) {
        (
            "http://127.0.0.1:9000".into(),
            "pgvisor-backups".into(),
            7,
            Some(10),
        )
    }
}

/// In-memory / mock backup service for standalone dashboard operation and testing.
pub struct StandaloneBackupService {
    backups: Arc<RwLock<Vec<BasebackupMeta>>>,
    endpoint: String,
    bucket: String,
}

impl StandaloneBackupService {
    pub fn new() -> Self {
        let initial = vec![
            BasebackupMeta::new(
                "snap-20260904-200000",
                Utc::now() - chrono::Duration::hours(5),
                BackupType::Full,
                "000000010000000000000001",
                14_850_000,
            )
            .with_label(Some("pre-migration-snapshot".into()))
            .with_stop_wal(Some("000000010000000000000002".into()))
            .with_source_node(Some("pgvisor-node2".into())),
            BasebackupMeta::new(
                "snap-20260904-210000",
                Utc::now() - chrono::Duration::hours(4),
                BackupType::Incremental,
                "000000010000000000000003",
                2_450_000,
            )
            .with_stop_wal(Some("000000010000000000000004".into()))
            .with_source_node(Some("pgvisor-node3".into()))
            .with_last_transaction(
                Some(Utc::now() - chrono::Duration::hours(4)),
                Some("10050".into()),
            ),
        ];
        Self {
            backups: Arc::new(RwLock::new(initial)),
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "pgvisor-backups".into(),
        }
    }
}

impl Default for StandaloneBackupService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BackupService for StandaloneBackupService {
    async fn list_backups(&self) -> Result<Vec<BasebackupMeta>, String> {
        let mut list = self.backups.read().await.clone();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(list)
    }

    async fn create_backup(
        &self,
        backup_type: BackupType,
        label: Option<String>,
    ) -> Result<BasebackupMeta, String> {
        let now = Utc::now();
        let snapshot_id = generate_snapshot_id(label.as_deref(), now);
        let bytes = match backup_type {
            BackupType::Full => 15_200_000,
            BackupType::Incremental => 1_850_000,
        };
        let mut meta = BasebackupMeta::new(
            snapshot_id,
            now,
            backup_type,
            "000000010000000000000010",
            bytes,
        )
        .with_label(label)
        .with_stop_wal(Some("000000010000000000000011".into()))
        .with_source_node(Some("pgvisor-node2".into()));
        meta.backup_finish_date = Some(now);
        meta.timeline = Some(1);

        let mut lock = self.backups.write().await;
        lock.push(meta.clone());

        let b_type_str = match backup_type {
            BackupType::Full => "FULL BACKUP",
            BackupType::Incremental => "INCREMENTAL BACKUP",
        };
        let backup_name = meta.label.as_deref().unwrap_or(&meta.snapshot_id);
        pgvisor_core::log_highlight(&pgvisor_core::format_backup_highlight(
            b_type_str,
            backup_name,
        ));

        Ok(meta)
    }

    async fn restore_backup(
        &self,
        snapshot_id: &str,
        target_time: Option<String>,
    ) -> Result<String, String> {
        let lock = self.backups.read().await;
        if let Some(b) = lock.iter().find(|b| b.snapshot_id == snapshot_id) {
            let b_type_str = match b.backup_type {
                BackupType::Full => "FULL BACKUP",
                BackupType::Incremental => "INCREMENTAL BACKUP",
            };
            let backup_name = b.label.as_deref().unwrap_or(&b.snapshot_id);
            pgvisor_core::log_highlight(&pgvisor_core::format_restore_highlight(
                b_type_str,
                backup_name,
                target_time.as_deref(),
            ));

            let detail = target_time
                .map(|t| format!(" (PITR target: {})", t))
                .unwrap_or_default();
            Ok(format!(
                "Snapshot {} successfully restored{}",
                snapshot_id, detail
            ))
        } else {
            Err(format!("Snapshot {} not found", snapshot_id))
        }
    }

    async fn delete_backup(&self, snapshot_id: &str) -> Result<(), String> {
        let mut lock = self.backups.write().await;
        let before_len = lock.len();
        lock.retain(|b| b.snapshot_id != snapshot_id);
        if lock.len() == before_len {
            Err(format!("Snapshot {} not found", snapshot_id))
        } else {
            Ok(())
        }
    }

    async fn get_backup_archive(
        &self,
        snapshot_id: &str,
    ) -> Result<(BasebackupMeta, Vec<u8>), String> {
        let lock = self.backups.read().await;
        let meta = lock
            .iter()
            .find(|b| b.snapshot_id == snapshot_id)
            .cloned()
            .ok_or_else(|| format!("Snapshot {} not found", snapshot_id))?;

        let dummy_tar_gz = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
        Ok((meta, dummy_tar_gz))
    }

    fn storage_info(&self) -> (String, String, u32, Option<usize>) {
        (self.endpoint.clone(), self.bucket.clone(), 7, Some(10))
    }
}

/// GET /backups -> Renders backup management page
pub async fn get_backups_page(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let list = state
        .backup_service
        .list_backups()
        .await
        .unwrap_or_default();
    let (storage_endpoint, storage_bucket, retention_days, keep_count) =
        state.backup_service.storage_info();

    let total_bytes: u64 = list.iter().map(|b| b.total_bytes).sum();
    let latest_backup = list
        .first()
        .map(|b| b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let summary = BackupOverviewSummary {
        total_backups: list.len(),
        latest_backup,
        total_size_pretty: format_bytes(total_bytes),
        retention_days,
        keep_count,
        storage_endpoint,
        storage_bucket,
    };

    let backup_items: Vec<BackupItemView> = list.iter().map(BackupItemView::from_meta).collect();

    let template = BackupsTemplate {
        backups: &backup_items,
        summary: &summary,
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render backups template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /api/backups -> JSON list of physical basebackup snapshots
pub async fn api_list_backups(
    State(state): State<Arc<DashboardState>>,
) -> Result<Json<Vec<BasebackupMeta>>, (StatusCode, String)> {
    state
        .backup_service
        .list_backups()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// POST /api/backups -> Triggers creation of a physical basebackup
pub async fn api_create_backup(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<CreateBackupRequest>,
) -> Result<Json<BasebackupMeta>, (StatusCode, String)> {
    let b_type = payload.backup_type.unwrap_or(BackupType::Full);
    let meta = state
        .backup_service
        .create_backup(b_type, payload.label)
        .await
        .map_err(|e| {
            if e.contains("already in progress") {
                (StatusCode::CONFLICT, e)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;

    {
        let mut overview = state.overview.write().await;
        overview.total_backups += 1;
        overview.last_backup_at = Some(meta.created_at);
    }

    Ok(Json(meta))
}

/// GET /api/backups/:id/download -> Streams compressed basebackup archive (.tar.gz)
pub async fn api_download_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (meta, tar_bytes) = state
        .backup_service
        .get_backup_archive(&snapshot_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    let filename = format!("{}.tar.gz", meta.snapshot_id);
    let disposition = format!("attachment; filename=\"{}\"", filename);

    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "application/gzip".to_string(),
        ),
        (axum::http::header::CONTENT_DISPOSITION, disposition),
    ];

    Ok((headers, tar_bytes))
}

/// POST /api/backups/:id/restore -> Triggers restore of database from snapshot
pub async fn api_restore_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
    Json(payload): Json<RestoreBackupRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target_time = if let Some(raw) = payload.recovery_target_time.as_deref() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            // Validate against snapshot metadata using shared core validator
            let backups = state
                .backup_service
                .list_backups()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

            let parsed = if let Some(snap) = backups
                .iter()
                .find(|b| b.snapshot_id == snapshot_id || b.label.as_deref() == Some(&snapshot_id))
            {
                pgvisor_core::backup::validate_recovery_target_time(
                    trimmed,
                    snap,
                    &backups,
                    Utc::now(),
                )
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
            } else {
                parse_target_timestamp(trimmed).map_err(|e| (StatusCode::BAD_REQUEST, e))?
            };
            Some(parsed.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        }
    } else {
        None
    };

    let msg = state
        .backup_service
        .restore_backup(&snapshot_id, target_time)
        .await
        .map_err(|e| {
            if e.contains("already in progress") {
                (StatusCode::CONFLICT, e)
            } else if e.contains("cannot be in the future")
                || e.contains("cannot roll backward")
                || e.contains("earlier than snapshot")
                || e.contains("Invalid timestamp")
            {
                (StatusCode::BAD_REQUEST, e)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;

    Ok(Json(serde_json::json!({
        "status": "success",
        "message": msg,
        "snapshot_id": snapshot_id
    })))
}

/// POST /api/backups/quick-restore -> Resolves best basebackup snapshot and restores to target time
pub async fn api_quick_restore(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<QuickRestoreRequest>,
) -> Result<Json<QuickRestoreResponse>, (StatusCode, String)> {
    let (best, msg) = state
        .backup_service
        .quick_restore(&payload.recovery_target_time)
        .await
        .map_err(|e| {
            if e.contains("already in progress") {
                (StatusCode::CONFLICT, e)
            } else {
                (StatusCode::BAD_REQUEST, e)
            }
        })?;

    Ok(Json(QuickRestoreResponse {
        status: "success".to_string(),
        message: msg,
        snapshot_id: best.snapshot_id,
        recovery_target_time: payload.recovery_target_time,
        snapshot_created_at: best.created_at.to_rfc3339(),
    }))
}

/// GET /api/backups/best?target_time=... -> Returns information on the best snapshot for a target time
pub async fn api_find_best_backup(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<BestBackupQuery>,
) -> Result<Json<BestBackupResponse>, (StatusCode, String)> {
    let parsed_target =
        parse_target_timestamp(&query.target_time).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let backups = state
        .backup_service
        .list_backups()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let best = find_best_backup_snapshot(&backups, parsed_target)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let backup_type = match best.backup_type {
        BackupType::Full => "full".to_string(),
        BackupType::Incremental => "incremental".to_string(),
    };

    Ok(Json(BestBackupResponse {
        snapshot_id: best.snapshot_id.clone(),
        created_at: best.created_at.to_rfc3339(),
        backup_type,
        label: best.label.clone(),
    }))
}

/// DELETE /api/backups/:id -> Deletes basebackup snapshot from storage
pub async fn api_delete_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .backup_service
        .delete_backup(&snapshot_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    {
        let mut overview = state.overview.write().await;
        if overview.total_backups > 0 {
            overview.total_backups -= 1;
        }
    }

    Ok(Json(serde_json::json!({
        "status": "deleted",
        "snapshot_id": snapshot_id
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target_timestamp_normalization() {
        // Single digit second normalization (e.g. user typed 00:35:0)
        let dt = parse_target_timestamp("2026-09-14 03:00:0")
            .expect("Must parse normalized single-digit second");
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-14 03:00:00"
        );

        // Standard format
        let dt2 =
            parse_target_timestamp("2026-09-14 03:05:12").expect("Must parse standard timestamp");
        assert_eq!(
            dt2.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-14 03:05:12"
        );

        // Future timestamp rejection
        let future_time = (Utc::now() + chrono::Duration::hours(2))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let err = parse_target_timestamp(&future_time).unwrap_err();
        assert!(err.contains("cannot be in the future"));
    }

    #[test]
    fn test_parse_target_timestamp_malformed_and_empty() {
        assert!(parse_target_timestamp("").is_err());
        assert!(parse_target_timestamp("   ").is_err());
        assert!(parse_target_timestamp("invalid-date").is_err());
        assert!(parse_target_timestamp("2026-99-99 99:99:99").is_err());
    }

    #[test]
    fn test_find_best_backup_snapshot_too_old_rejected() {
        let snap_time = chrono::DateTime::parse_from_rfc3339("2026-09-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut meta = BasebackupMeta::new(
            "snap-1000",
            snap_time,
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );
        meta.label = Some("test-label".to_string());
        meta.source_node = Some("pgvisor-node1".to_string());

        let backups = vec![meta];

        // Target time 1 hour before snapshot creation (too old)
        let too_old_target = snap_time - chrono::Duration::hours(1);
        let err = find_best_backup_snapshot(&backups, too_old_target).unwrap_err();
        assert!(err.contains("No basebackup snapshot found prior to target time"));
        assert!(err.contains("cannot roll backward"));
    }

    #[test]
    fn test_find_best_backup_snapshot_selection_and_empty() {
        // Empty backups list
        let empty: Vec<BasebackupMeta> = vec![];
        let now = Utc::now();
        assert!(find_best_backup_snapshot(&empty, now).is_err());

        let t1 = chrono::DateTime::parse_from_rfc3339("2026-09-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let t2 = chrono::DateTime::parse_from_rfc3339("2026-09-15T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let snap1 = BasebackupMeta::new(
            "snap-1",
            t1,
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );
        let snap2 = BasebackupMeta::new(
            "snap-2",
            t2,
            BackupType::Incremental,
            "000000010000000000000002",
            512,
        );

        let backups = vec![snap1, snap2];

        // Target between t1 and t2 selects snap-1
        let target_mid = t1 + chrono::Duration::minutes(30);
        let best = find_best_backup_snapshot(&backups, target_mid).unwrap();
        assert_eq!(best.snapshot_id, "snap-1");

        // Target after t2 selects snap-2
        let target_after = t2 + chrono::Duration::minutes(15);
        let best2 = find_best_backup_snapshot(&backups, target_after).unwrap();
        assert_eq!(best2.snapshot_id, "snap-2");
    }

    #[tokio::test]
    async fn test_quick_restore_formats_rfc3339_for_postgres() {
        let svc = StandaloneBackupService::new();
        let backups = svc.list_backups().await.unwrap();
        let target_dt = backups[0].created_at + chrono::Duration::minutes(10);
        let target_rfc3339 = target_dt.to_rfc3339();
        let expected_pg_format = target_dt.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        let (_best, msg) = svc.quick_restore(&target_rfc3339).await.unwrap();
        assert!(
            msg.contains(&expected_pg_format),
            "Expected formatted UTC timestamp '{expected_pg_format}' in restore message, got: {msg}"
        );
        assert!(
            !msg.contains(&target_rfc3339),
            "Must not pass raw RFC 3339 '{target_rfc3339}' to restore_backup: {msg}"
        );
    }

    #[test]
    fn test_backup_item_view_from_meta_mapping() {
        let now = Utc::now();
        let meta_with_tx = BasebackupMeta::new(
            "snap-tx",
            now,
            BackupType::Full,
            "000000010000000000000001",
            1024,
        )
        .with_last_transaction(Some(now), Some("9999".to_string()));

        let view_with_tx = BackupItemView::from_meta(&meta_with_tx);
        assert_eq!(view_with_tx.snapshot_id, "snap-tx");
        assert_eq!(view_with_tx.backup_type, "full");
        assert_eq!(
            view_with_tx.last_transaction_time.as_deref(),
            Some(now.format("%Y-%m-%d %H:%M:%S UTC").to_string().as_str())
        );
        assert_eq!(view_with_tx.last_transaction_xid.as_deref(), Some("9999"));

        let meta_without_tx = BasebackupMeta::new(
            "snap-notx",
            now,
            BackupType::Incremental,
            "000000010000000000000002",
            2048,
        );

        let view_without_tx = BackupItemView::from_meta(&meta_without_tx);
        assert_eq!(view_without_tx.snapshot_id, "snap-notx");
        assert_eq!(view_without_tx.backup_type, "incremental");
        assert_eq!(view_without_tx.last_transaction_time, None);
        assert_eq!(view_without_tx.last_transaction_xid, None);
    }
}
