use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pgvisor_core::api::SidecarRole;
use pgvisor_core::audit::AuditEventKind;
use pgvisor_core::backup::{
    create_simulated_basebackup, generate_snapshot_id, inspect_and_prepare_import_archive,
    process_basebackup_archive, BackupManager, BackupType, BasebackupMeta, RecoveryTarget,
};
use tempfile::{tempdir, NamedTempFile};
use tokio::fs;
use tracing::{debug, info, warn};

use super::helpers::{find_best_backup_snapshot, parse_target_timestamp, validate_restore_target};
use crate::api::cluster_actions::post_peer_control;
use crate::api::peer_probe::{probe_all_peers, PeerProbe};
use crate::api::{ApiError, ApiState};
use crate::control::{
    acquire_backup_lock_local, release_backup_lock_local, RestorePayload,
};

const MAX_RESYNC_ATTEMPTS: u32 = 5;
const FALLBACK_XID: &str = "1000";

fn require_backup_manager(state: &ApiState) -> Result<&Arc<BackupManager>, ApiError> {
    state.sidecar.backup_manager.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable("BackupManager is not configured on this sidecar node".into())
    })
}

async fn acquire_leader_lock(state: &ApiState) -> Result<String, ApiError> {
    acquire_backup_lock_local(&state.sidecar)
        .await
        .map(|resp| resp.lock_token)
        .map_err(|(status, msg)| match status {
            axum::http::StatusCode::CONFLICT => ApiError::Conflict(msg),
            axum::http::StatusCode::SERVICE_UNAVAILABLE => {
                ApiError::NotLeader { leader_url: None }
            }
            _ => ApiError::Internal(msg),
        })
}

fn split_host_port(addr: &str, default_port: u16) -> (String, u16) {
    let mut parts = addr.split(':');
    let host = parts.next().unwrap_or("127.0.0.1").to_string();
    let port = parts
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(default_port);
    (host, port)
}

fn select_backup_target_from_probes(state: &ApiState, probes: &[PeerProbe]) -> (String, u16, bool) {
    let standby = probes.iter().find(|p| {
        !p.is_self
            && p.status.as_ref().is_some_and(|s| {
                s.status == "running" && SidecarRole::from_role_str(&s.role) == SidecarRole::Standby
            })
    });
    if let Some(peer) = standby {
        let (host, port) = split_host_port(&peer.pg_address, state.sidecar.pg_port);
        info!(
            target_node = %host,
            port,
            "Performing physical basebackup on follower standby to reduce leader load"
        );
        return (host, port, true);
    }

    let (host, port) = split_host_port(&state.pg_addr, state.sidecar.pg_port);
    warn!(
        target_node = %host,
        port,
        "No healthy standby replicas available; falling back to leader node for basebackup"
    );
    (host, port, false)
}

fn format_recovery_target_label(target: Option<&RecoveryTarget>) -> Option<&str> {
    target.map(|t| match t {
        RecoveryTarget::Time(s) | RecoveryTarget::Xid(s) => s.as_str(),
        RecoveryTarget::Latest => "latest",
    })
}

fn build_restore_payload(snapshot_id: &str, target: Option<&RecoveryTarget>) -> RestorePayload {
    let (recovery_target_time, recovery_target_xid) = match target {
        Some(RecoveryTarget::Time(t)) => (Some(t.clone()), None),
        Some(RecoveryTarget::Xid(x)) => (None, Some(x.clone())),
        Some(RecoveryTarget::Latest) | None => (None, None),
    };
    RestorePayload {
        snapshot_id: snapshot_id.to_string(),
        recovery_target_time,
        recovery_target_xid,
        // Cloning optional enum value keeps payload compatible with both legacy fields and RecoveryTarget.
        recovery_target: target.cloned(),
    }
}

async fn fetch_last_committed_transaction(state: &ApiState) -> Option<(DateTime<Utc>, String)> {
    let queries = [
        "SELECT (pg_last_committed_xact()).xid::text, to_char((pg_last_committed_xact()).timestamp AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
        "SELECT pg_snapshot_xmax(pg_current_snapshot())::text, to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    ];

    for sql in queries {
        match state
            .sql_runner
            .execute_as_user(&state.pg_superuser, None, sql, 1)
            .await
        {
            Ok(result) => {
                if let Some(row) = result.rows.first() {
                    if row.len() >= 2 {
                        let xid = row[0].trim();
                        let ts_str = row[1].trim();
                        if !xid.is_empty()
                            && xid != "NULL"
                            && !ts_str.is_empty()
                            && ts_str != "NULL"
                        {
                            if let Ok(parsed) = DateTime::parse_from_rfc3339(ts_str) {
                                return Some((parsed.with_timezone(&Utc), xid.to_string()));
                            }
                        }
                    }
                }
            }
            Err(err) => {
                debug!(?err, "Transaction metadata query failed; trying next fallback");
            }
        }
    }
    None
}

async fn run_pg_basebackup_or_simulated(
    state: &ApiState,
    host: &str,
    port: u16,
    is_follower: bool,
    meta: &mut BasebackupMeta,
) -> Result<Vec<u8>, ApiError> {
    let tmp_dir =
        tempdir().map_err(|e| ApiError::Internal(format!("Failed to create temp dir: {e}")))?;
    let tmp_path = tmp_dir.path();

    let status_res = tokio::process::Command::new("pg_basebackup")
        .arg("-h")
        .arg(host)
        .arg("-p")
        .arg(port.to_string())
        .arg("-U")
        .arg(&state.pg_superuser)
        .arg("-Ft")
        .arg("-z")
        .arg("-X")
        .arg("fetch")
        .arg("-D")
        .arg(tmp_path)
        .status()
        .await;

    if let Some((tx_time, tx_xid)) = fetch_last_committed_transaction(state).await {
        meta.last_transaction_time = Some(tx_time);
        meta.last_transaction_xid = Some(tx_xid);
    }

    if let Ok(status) = status_res {
        let base_tar = tmp_path.join("base.tar.gz");
        if status.success() && base_tar.exists() {
            let bytes = fs::read(&base_tar).await.map_err(|e| {
                ApiError::Internal(format!("Failed to read base.tar.gz: {e}"))
            })?;
            meta.backup_finish_date = Some(Utc::now());
            if meta.last_transaction_time.is_none() {
                meta.last_transaction_time =
                    Some(meta.backup_start_date.unwrap_or_else(Utc::now));
                meta.last_transaction_xid = Some(FALLBACK_XID.to_string());
            }

            let mut final_tar = Vec::new();
            process_basebackup_archive(bytes.as_slice(), &mut final_tar, meta).map_err(|e| {
                ApiError::Internal(format!("Failed to process basebackup archive: {e}"))
            })?;
            meta.total_bytes = final_tar.len() as u64;
            if meta.stop_wal.is_none() {
                meta.stop_wal = Some(meta.start_wal.clone());
            }
            info!(
                bytes = meta.total_bytes,
                is_follower,
                timeline = ?meta.timeline,
                "Captured physical basebackup archive"
            );
            return Ok(final_tar);
        }
        warn!(
            ?status,
            "pg_basebackup did not produce base.tar.gz; falling back to simulated snapshot"
        );
    } else {
        warn!("pg_basebackup binary unavailable; falling back to simulated snapshot");
    }

    meta.backup_finish_date = Some(Utc::now());
    if meta.last_transaction_time.is_none() {
        meta.last_transaction_time = Some(Utc::now());
        meta.last_transaction_xid = Some(FALLBACK_XID.to_string());
    }
    let simulated = create_simulated_basebackup(meta)
        .map_err(|e| ApiError::Internal(format!("Failed to create simulated basebackup: {e}")))?;
    meta.total_bytes = simulated.len() as u64;
    Ok(simulated)
}

async fn notify_standbys_control(
    state: &ApiState,
    standbys: &[&PeerProbe],
    path: &str,
) {
    for peer in standbys {
        if let Err(err) = post_peer_control(state, &peer.peer_url, path, None).await {
            warn!(
                peer_url = %peer.peer_url,
                path,
                ?err,
                "Best-effort standby restore control notification failed"
            );
        }
    }
}

async fn resync_standby_with_retries(
    state: &ApiState,
    peer_url: &str,
    primary_conninfo: &str,
) {
    let payload = serde_json::json!({ "primary_conninfo": primary_conninfo });
    let retry_delay = state.leader_restore_executor.resync_retry_delay();
    for attempt in 1..=MAX_RESYNC_ATTEMPTS {
        match post_peer_control(state, peer_url, "/control/resync", Some(payload.clone())).await {
            Ok(_) => {
                info!(%peer_url, attempt, "Standby replica successfully re-synced");
                return;
            }
            Err(err) => {
                warn!(
                    %peer_url,
                    attempt,
                    ?err,
                    "Standby re-sync failed, retrying"
                );
            }
        }
        if attempt < MAX_RESYNC_ATTEMPTS && !retry_delay.is_zero() {
            tokio::time::sleep(retry_delay).await;
        }
    }
    warn!(%peer_url, "Standby re-sync failed after all retry attempts");
}

/// Lists all registered physical basebackups sorted newest-first.
pub async fn list_backups(state: &ApiState) -> Result<Vec<BasebackupMeta>, ApiError> {
    let bm = require_backup_manager(state)?;
    let mut list = bm
        .list_basebackups()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to list basebackups: {e}")))?;
    list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(list)
}

/// Fetches metadata and gzip tarball bytes for a single snapshot.
pub async fn get_backup_archive(
    state: &ApiState,
    snapshot_id: &str,
) -> Result<(BasebackupMeta, Vec<u8>), ApiError> {
    let bm = require_backup_manager(state)?;
    bm.get_basebackup(snapshot_id)
        .await
        .map_err(|e| ApiError::NotFound(format!("Snapshot '{snapshot_id}' not found: {e}")))
}

/// Creates a physical basebackup snapshot under the cluster backup lock and records an audit event.
pub async fn create_backup(
    state: &ApiState,
    backup_type: BackupType,
    label: Option<String>,
    actor: &str,
) -> Result<BasebackupMeta, ApiError> {
    let bm = require_backup_manager(state)?;
    let lock_token = acquire_leader_lock(state).await?;
    let res = create_backup_locked(state, bm, backup_type, label, actor).await;
    release_backup_lock_local(&state.sidecar, &lock_token).await;
    res
}

async fn create_backup_locked(
    state: &ApiState,
    bm: &Arc<BackupManager>,
    backup_type: BackupType,
    label: Option<String>,
    actor: &str,
) -> Result<BasebackupMeta, ApiError> {
    let probes = probe_all_peers(state).await;
    let (host, port, is_follower) = select_backup_target_from_probes(state, &probes);

    let start_time = Utc::now();
    let snapshot_id = generate_snapshot_id(label.as_deref(), start_time);
    let mut meta = BasebackupMeta::new(
        &snapshot_id,
        start_time,
        backup_type,
        "000000010000000000000001",
        0,
    )
    .with_label(label.clone())
    .with_source_node(Some(host.clone()));

    let tar_data =
        run_pg_basebackup_or_simulated(state, &host, port, is_follower, &mut meta).await?;

    bm.save_basebackup(&meta, tar_data)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to save basebackup to OpenDAL: {e}")))?;

    if let Err(err) = bm
        .prune_retention(
            state.backup_retention.keep_count,
            Some(state.backup_retention.retention_days),
        )
        .await
    {
        warn!(?err, "Failed to prune backup retention");
    }

    let b_type_str = match backup_type {
        BackupType::Full => "FULL BACKUP",
        BackupType::Incremental => "INCREMENTAL BACKUP",
    };
    let backup_name = meta.label.as_deref().unwrap_or(&meta.snapshot_id);
    pgvisor_core::log_highlight(&pgvisor_core::format_backup_highlight(
        b_type_str,
        backup_name,
    ));

    let label_str = meta.label.as_deref().unwrap_or("none");
    state
        .audit_log
        .append(
            AuditEventKind::BackupCreated,
            None,
            Some(&host),
            format!(
                "Backup '{}' ({:?}, label: '{}', {} bytes) created by actor '{}'",
                meta.snapshot_id, meta.backup_type, label_str, meta.total_bytes, actor
            ),
            None,
        )
        .await;

    Ok(meta)
}

/// Validates and imports an uploaded basebackup archive under the cluster backup lock.
pub async fn import_backup(
    state: &ApiState,
    archive_path: &Path,
    label: Option<String>,
    actor: &str,
) -> Result<BasebackupMeta, ApiError> {
    let bm = require_backup_manager(state)?;
    let lock_token = acquire_leader_lock(state).await?;
    let res = import_backup_locked(state, bm, archive_path, label, actor).await;
    release_backup_lock_local(&state.sidecar, &lock_token).await;
    res
}

async fn import_backup_locked(
    state: &ApiState,
    bm: &Arc<BackupManager>,
    archive_path: &Path,
    label: Option<String>,
    actor: &str,
) -> Result<BasebackupMeta, ApiError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| ApiError::Internal(format!("Failed to open uploaded backup file: {e}")))?;
    let out_temp_file = NamedTempFile::new()
        .map_err(|e| ApiError::Internal(format!("Failed to create temp output file: {e}")))?;
    let out_path = out_temp_file.path().to_path_buf();
    let out_writer = std::fs::File::create(&out_path)
        .map_err(|e| ApiError::Internal(format!("Failed to open temp output file: {e}")))?;

    let mut meta = inspect_and_prepare_import_archive(file, out_writer, label.as_deref())
        .map_err(|e| ApiError::BadRequest(format!("Invalid backup archive: {e}")))?;

    let tar_data = fs::read(&out_path)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to read processed backup archive: {e}")))?;
    meta.total_bytes = tar_data.len() as u64;

    bm.save_basebackup(&meta, tar_data)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to save imported basebackup: {e}")))?;

    let backup_name = meta.label.as_deref().unwrap_or(&meta.snapshot_id);
    pgvisor_core::log_highlight(&pgvisor_core::format_backup_highlight(
        "IMPORTED BACKUP",
        backup_name,
    ));

    state
        .audit_log
        .append(
            AuditEventKind::BackupCreated,
            None,
            Some("imported"),
            format!(
                "Imported {:?} snapshot '{}' (size: {} bytes) by actor '{}'",
                meta.backup_type, meta.snapshot_id, meta.total_bytes, actor
            ),
            None,
        )
        .await;

    Ok(meta)
}

/// Deletes a snapshot from OpenDAL storage under the cluster backup lock.
pub async fn delete_backup(
    state: &ApiState,
    snapshot_id: &str,
    actor: &str,
) -> Result<(), ApiError> {
    let bm = require_backup_manager(state)?;
    let lock_token = acquire_leader_lock(state).await?;
    let res = bm
        .delete_basebackup(snapshot_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete basebackup: {e}")));
    release_backup_lock_local(&state.sidecar, &lock_token).await;

    res?;
    state
        .audit_log
        .append(
            AuditEventKind::BackupCreated,
            Some(state.sidecar.node_id),
            Some(&state.pg_addr),
            format!("Snapshot '{}' deleted by actor '{}'", snapshot_id, actor),
            None,
        )
        .await;
    Ok(())
}

/// Orchestrates a cluster-wide restore on the leader and re-syncs all standby peers.
pub async fn restore_backup(
    state: &ApiState,
    snapshot_id: &str,
    target: Option<RecoveryTarget>,
    actor: &str,
) -> Result<String, ApiError> {
    let bm = require_backup_manager(state)?;
    let lock_token = acquire_leader_lock(state).await?;
    let res = restore_backup_locked(state, bm, snapshot_id, target, actor).await;
    release_backup_lock_local(&state.sidecar, &lock_token).await;
    res
}

async fn restore_backup_locked(
    state: &ApiState,
    bm: &Arc<BackupManager>,
    snapshot_id: &str,
    target: Option<RecoveryTarget>,
    actor: &str,
) -> Result<String, ApiError> {
    if let Some(ref t) = target {
        let list = bm.list_basebackups().await.unwrap_or_default();
        validate_restore_target(snapshot_id, t, &list)?;
    }

    let probes = probe_all_peers(state).await;
    let standbys: Vec<&PeerProbe> = probes.iter().filter(|p| !p.is_self).collect();

    // Pause failover on standbys before stopping/rewinding the leader
    notify_standbys_control(state, &standbys, "/control/prepare-restore").await;

    let payload = build_restore_payload(snapshot_id, target.as_ref());
    if let Err(err) = state
        .leader_restore_executor
        .execute_restore(&state.sidecar, payload)
        .await
    {
        notify_standbys_control(state, &standbys, "/control/cancel-restore").await;
        return Err(err);
    }

    let leader_host = probes
        .iter()
        .find(|p| p.is_self)
        .map(|p| split_host_port(&p.pg_address, state.sidecar.pg_port).0)
        .unwrap_or_else(|| split_host_port(&state.pg_addr, state.sidecar.pg_port).0);
    let primary_conninfo = format!(
        "host={leader_host} port={} user={}",
        state.sidecar.pg_port, state.pg_superuser
    );

    for standby in &standbys {
        resync_standby_with_retries(state, &standby.peer_url, &primary_conninfo).await;
    }

    let target_display = format_recovery_target_label(target.as_ref());
    let detail = target_display
        .map(|t| format!(" with PITR target '{t}'"))
        .unwrap_or_default();

    state
        .audit_log
        .append(
            AuditEventKind::BackupRestored,
            Some(state.sidecar.node_id),
            Some(&leader_host),
            format!(
                "Snapshot '{}' restored to cluster{} by actor '{}'",
                snapshot_id, detail, actor
            ),
            target_display.map(String::from),
        )
        .await;

    Ok(format!(
        "Snapshot {snapshot_id} successfully restored to cluster{detail}"
    ))
}

/// Resolves the best snapshot on or before `target_time` and restores to it.
pub async fn quick_restore(
    state: &ApiState,
    target_time: &str,
    actor: &str,
) -> Result<(BasebackupMeta, String), ApiError> {
    let parsed_target = parse_target_timestamp(target_time)?;
    let backups = list_backups(state).await?;
    // Clone metadata before running async restore because list borrows backups slice.
    let best = find_best_backup_snapshot(&backups, parsed_target)?.clone();
    let formatted_target = parsed_target.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let msg = restore_backup(
        state,
        &best.snapshot_id,
        Some(RecoveryTarget::Time(formatted_target)),
        actor,
    )
    .await?;
    Ok((best, msg))
}
