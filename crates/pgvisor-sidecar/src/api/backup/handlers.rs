use std::collections::HashMap;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use chrono::Utc;
use pgvisor_core::api::{
    BestBackupQuery, BestBackupResponse, CreateBackupRequest, PitrWindowResponse,
    QuickRestoreRequest, QuickRestoreResponse, RestoreBackupRequest,
};
use pgvisor_core::backup::{BackupType, BasebackupMeta};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

use super::helpers::{find_best_backup_snapshot, max_upload_bytes, parse_target_timestamp};
use super::orchestrator;
use crate::api::members::require_leader;
use crate::api::{ApiError, ApiState, AuthenticatedUser};

/// `GET /api/backups` -> JSON list of physical basebackup snapshots.
pub async fn list_backups(
    State(state): State<ApiState>,
) -> Result<Json<Vec<BasebackupMeta>>, ApiError> {
    orchestrator::list_backups(&state).await.map(Json)
}

/// `POST /api/backups` -> Triggers creation of a physical basebackup on the leader.
pub async fn create_backup(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(payload): Json<CreateBackupRequest>,
) -> Result<Json<BasebackupMeta>, ApiError> {
    require_leader(&state).await?;
    let b_type = payload.backup_type.unwrap_or(BackupType::Full);
    orchestrator::create_backup(&state, b_type, payload.label, &user.username)
        .await
        .map(Json)
}

/// `GET /api/backups/:snapshot_id/download` -> Streams compressed basebackup archive (`.tar.gz`).
pub async fn download_backup(
    State(state): State<ApiState>,
    Path(snapshot_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let (meta, tar_bytes) = orchestrator::get_backup_archive(&state, &snapshot_id).await?;
    let disposition = format!("attachment; filename=\"{}.tar.gz\"", meta.snapshot_id);

    let mut headers = HeaderMap::new();
    if let Ok(val) = header::HeaderValue::from_str("application/gzip") {
        headers.insert(header::CONTENT_TYPE, val);
    }
    if let Ok(val) = header::HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, val);
    }
    Ok((StatusCode::OK, headers, tar_bytes))
}

/// `POST /api/backups/upload` -> Ingests a multipart basebackup archive and optionally restores it.
pub async fn upload_backup(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Query(query_params): Query<HashMap<String, String>>,
    mut multipart: Multipart,
) -> Result<Json<BasebackupMeta>, ApiError> {
    require_leader(&state).await?;

    let max_bytes = max_upload_bytes();
    let mut custom_label: Option<String> = None;
    let mut restore_requested = query_params
        .get("restore")
        .is_some_and(|v| v == "true" || v == "1");

    let temp_file = NamedTempFile::new()
        .map_err(|e| ApiError::Internal(format!("Failed to create temporary upload file: {e}")))?;
    let temp_path = temp_file.path().to_path_buf();
    let mut uploaded_bytes = 0u64;
    let mut file_received = false;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Failed reading multipart field: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "label" {
            if let Ok(text) = field.text().await {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    custom_label = Some(trimmed.to_string());
                }
            }
        } else if name == "restore" {
            if let Ok(text) = field.text().await {
                let trimmed = text.trim();
                if trimmed == "true" || trimmed == "1" {
                    restore_requested = true;
                }
            }
        } else if name == "file" || name == "backup" || name == "archive" {
            file_received = true;
            let mut async_file = tokio::fs::File::create(&temp_path)
                .await
                .map_err(|e| ApiError::Internal(format!("Failed to create upload file: {e}")))?;

            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| ApiError::BadRequest(format!("Failed reading upload chunk: {e}")))?
            {
                uploaded_bytes += chunk.len() as u64;
                if uploaded_bytes > max_bytes as u64 {
                    // Best-effort temp cleanup before returning payload-too-large error.
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return Err(ApiError::PayloadTooLarge(format!(
                        "Upload exceeds maximum allowed size of {max_bytes} bytes"
                    )));
                }
                async_file.write_all(&chunk).await.map_err(|e| {
                    ApiError::Internal(format!("Failed writing upload chunk: {e}"))
                })?;
            }
            async_file
                .flush()
                .await
                .map_err(|e| ApiError::Internal(format!("Failed flushing upload file: {e}")))?;
        }
    }

    if !file_received || uploaded_bytes == 0 {
        // Best-effort temp cleanup when multipart payload omitted the file field.
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(ApiError::BadRequest(
            "No backup archive file provided in multipart upload (expected field 'file')".into(),
        ));
    }

    let import_res =
        orchestrator::import_backup(&state, &temp_path, custom_label, &user.username).await;
    // NamedTempFile will drop shortly, explicitly remove async copy beforehand.
    let _ = tokio::fs::remove_file(&temp_path).await;
    let meta = import_res?;

    if restore_requested {
        orchestrator::restore_backup(&state, &meta.snapshot_id, None, &user.username)
            .await
            .map_err(|err| {
                ApiError::Internal(format!(
                    "Imported backup {}, but cluster restore failed: {}",
                    meta.snapshot_id, err
                ))
            })?;
    }

    Ok(Json(meta))
}

/// `POST /api/backups/:snapshot_id/restore` -> Triggers cluster-wide restore on the leader.
pub async fn restore_backup(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(snapshot_id): Path<String>,
    Json(payload): Json<RestoreBackupRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_leader(&state).await?;
    let target = payload.target();
    let msg = orchestrator::restore_backup(&state, &snapshot_id, target, &user.username).await?;

    Ok(Json(serde_json::json!({
        "status": "success",
        "message": msg,
        "snapshot_id": snapshot_id,
    })))
}

/// `POST /api/backups/quick-restore` -> Resolves the best snapshot and restores cluster to target time.
pub async fn quick_restore(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(payload): Json<QuickRestoreRequest>,
) -> Result<Json<QuickRestoreResponse>, ApiError> {
    require_leader(&state).await?;
    let (best, msg) =
        orchestrator::quick_restore(&state, &payload.recovery_target_time, &user.username).await?;

    Ok(Json(QuickRestoreResponse {
        status: "success".to_string(),
        message: msg,
        snapshot_id: best.snapshot_id,
        recovery_target_time: payload.recovery_target_time,
        snapshot_created_at: best.created_at.to_rfc3339(),
    }))
}

/// `GET /api/backups/best?target_time=...` -> Returns the best snapshot eligible for forward PITR.
pub async fn find_best_backup(
    State(state): State<ApiState>,
    Query(query): Query<BestBackupQuery>,
) -> Result<Json<BestBackupResponse>, ApiError> {
    let parsed_target = parse_target_timestamp(&query.target_time)?;
    let backups = orchestrator::list_backups(&state).await?;
    let best = find_best_backup_snapshot(&backups, parsed_target)?;

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

/// `DELETE /api/backups/:snapshot_id` -> Deletes basebackup snapshot from storage on the leader.
pub async fn delete_backup(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(snapshot_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_leader(&state).await?;
    orchestrator::delete_backup(&state, &snapshot_id, &user.username).await?;

    Ok(Json(serde_json::json!({
        "status": "deleted",
        "snapshot_id": snapshot_id,
    })))
}

/// `GET /api/backups/:snapshot_id/pitr-window` -> Returns PITR time boundaries for a snapshot.
pub async fn pitr_window(
    State(state): State<ApiState>,
    Path(snapshot_id): Path<String>,
) -> Result<Json<PitrWindowResponse>, ApiError> {
    let backups = orchestrator::list_backups(&state).await?;
    let snap = backups
        .iter()
        .find(|b| b.snapshot_id == snapshot_id || b.label.as_deref() == Some(&snapshot_id))
        .ok_or_else(|| ApiError::NotFound(format!("Snapshot {snapshot_id} not found")))?;

    let min_dt = snap
        .backup_finish_date
        .or(snap.backup_start_date)
        .unwrap_or(snap.created_at);
    let now = Utc::now();
    let max_dt = if now < min_dt { min_dt } else { now };
    let last_tx_dt = snap.last_transaction_time;

    Ok(Json(PitrWindowResponse {
        snapshot_id: snap.snapshot_id.clone(),
        min_time: min_dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        min_timestamp: min_dt.timestamp(),
        max_time: max_dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        max_timestamp: max_dt.timestamp(),
        last_transaction_time: last_tx_dt.map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
        last_transaction_timestamp: last_tx_dt.map(|t| t.timestamp()),
    }))
}
