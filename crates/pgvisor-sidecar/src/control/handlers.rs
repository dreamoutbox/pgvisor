use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use pgvisor_core::backup::BackupType;
use pgvisor_core::node::{NodeConfigResponse, NodeConfigType, NodeLogsResponse};
use pgvisor_core::{
    extract_node_name, format_become_leader_highlight, format_leader_down_highlight,
    format_restore_highlight, log_highlight,
};
use tracing::{info, warn};

use super::state::{
    AcquireBackupLockResponse, BackupLockInfo, EventsQuery, LogsQuery, ReleaseLockPayload,
    RepointPayload, RestorePayload, ResyncPayload, SidecarEventRecord, SidecarState,
    StatusResponse,
};

use crate::supervisor::ProcessStatus;

pub async fn handle_status(State(state): State<SidecarState>) -> impl IntoResponse {
    let supervisor_status = state.supervisor.status().await;
    let status_str = match supervisor_status {
        ProcessStatus::Running => "running",
        ProcessStatus::Stopped => "stopped",
        ProcessStatus::Fenced => "fenced",
        ProcessStatus::Restoring => "restoring",
    };
    let current_role = state.role.read().await.clone();
    let uptime_secs = state.supervisor.uptime_secs().await;
    let cpu_percent = state.system_metrics.cpu_percent();
    let (memory_used_bytes, memory_total_bytes) = state.system_metrics.memory_usage();

    Json(StatusResponse {
        node_id: state.node_id,
        role: current_role,
        status: status_str.to_string(),
        child_pid: state.supervisor.child_pid(),
        pg_version: state.pg_version.clone(),
        uptime_secs,
        cpu_percent,
        memory_used_bytes,
        memory_total_bytes,
    })
}

pub async fn handle_events(
    State(state): State<SidecarState>,
    Query(query): Query<EventsQuery>,
) -> Json<Vec<SidecarEventRecord>> {
    let since_id = query.since_id.unwrap_or(0);
    let q = state.events.read().await;
    let records: Vec<SidecarEventRecord> = q.iter().filter(|e| e.id > since_id).cloned().collect();
    Json(records)
}

/// GET /control/logs?limit=N -> Returns recent buffered PostgreSQL log lines
pub async fn handle_logs(
    State(state): State<SidecarState>,
    Query(query): Query<LogsQuery>,
) -> Json<NodeLogsResponse> {
    let limit = query.limit.unwrap_or(100).min(1000).max(1);
    let entries = state.supervisor.recent_logs(limit).await;
    let total_buffered = state.supervisor.total_buffered_logs().await;
    Json(NodeLogsResponse {
        node_id: state.node_id,
        total_buffered,
        entries,
    })
}

/// GET /control/config/:config_type -> Inspects specific configuration or diagnostic file safely
pub async fn handle_config(
    State(state): State<SidecarState>,
    Path(file_type_slug): Path<String>,
) -> Result<Json<NodeConfigResponse>, (StatusCode, Json<serde_json::Value>)> {
    let config_type = match NodeConfigType::from_slug(&file_type_slug) {
        Some(ct) => ct,
        None => {
            let supported: Vec<&str> = NodeConfigType::all().iter().map(|c| c.to_slug()).collect();
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid config file slug '{}'. Supported slugs: {:?}", file_type_slug, supported)
                })),
            ));
        }
    };

    match state.supervisor.read_node_file(config_type).await {
        Ok(mut resp) => {
            resp.node_id = state.node_id;
            Ok(Json(resp))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to read file: {}", e)
            })),
        )),
    }
}

pub async fn handle_promote(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    info!(
        node_id = state.node_id,
        "Handling manual/automated promotion request"
    );

    let old_conninfo = state
        .config
        .read()
        .await
        .primary_conninfo
        .clone()
        .unwrap_or_default();
    let old_node = extract_node_name(&old_conninfo);
    let old_leader = if old_node == "unknown"
        || old_node.is_empty()
        || old_node == format!("node{}", state.node_id)
    {
        "node1".to_string()
    } else {
        old_node
    };
    let my_node = format!("node{}", state.node_id);
    log_highlight(&format_become_leader_highlight(&old_leader, &my_node));

    state.supervisor.promote().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Promotion failed: {}", e)
            })),
        )
    })?;

    {
        let mut r = state.role.write().await;
        *r = "leader".to_string();
        let mut cfg = state.config.write().await;
        cfg.primary_conninfo = None;
    }

    state
        .record_event(
            "election_result",
            format!("Node {} successfully promoted to leader", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} successfully promoted to leader", state.node_id),
        "node_id": state.node_id,
        "role": "leader"
    })))
}

pub async fn handle_fence(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    warn!(node_id = state.node_id, "Handling fencing request");
    state.supervisor.fence().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Fencing failed: {}", e)
            })),
        )
    })?;

    {
        let mut r = state.role.write().await;
        *r = "fenced".to_string();
    }

    state
        .record_event(
            "election_result",
            format!("Node {} fenced immediately", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} fenced immediately", state.node_id),
        "node_id": state.node_id,
        "role": "fenced"
    })))
}

pub async fn handle_demote(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    info!(node_id = state.node_id, "Handling demotion request");
    state.supervisor.stop().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Demotion failed: {}", e)
            })),
        )
    })?;

    state.supervisor.set_status(ProcessStatus::Fenced).await;

    {
        let mut r = state.role.write().await;
        *r = "fenced".to_string();
    }

    state
        .record_event(
            "election_result",
            format!("Node {} demoted and stopped cleanly", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} successfully demoted and stopped cleanly", state.node_id),
        "node_id": state.node_id,
        "role": "fenced"
    })))
}

pub async fn handle_repoint(
    State(state): State<SidecarState>,
    Json(payload): Json<RepointPayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let current_role = state.role.read().await.clone();
    let current_status = state.supervisor.status().await;
    if current_role == "leader" && current_status == ProcessStatus::Running {
        info!(
            node_id = state.node_id,
            "Ignoring repoint request because node is currently running as leader"
        );
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "message": "Node is leader, ignoring repoint",
            "node_id": state.node_id,
            "role": "leader"
        })));
    }

    let mut target_conninfo = payload.primary_conninfo.clone();
    if !target_conninfo.contains("application_name=") {
        target_conninfo.push_str(&format!(" application_name=pgvisor-node{}", state.node_id));
    }

    if current_status == ProcessStatus::Running {
        let old_conninfo = state
            .config
            .read()
            .await
            .primary_conninfo
            .clone()
            .unwrap_or_default();
        let old_node = extract_node_name(&old_conninfo);
        let new_node = extract_node_name(&payload.primary_conninfo);
        let old_leader = if old_node == "unknown" || old_node.is_empty() || old_node == new_node {
            "node1".to_string()
        } else {
            old_node
        };
        log_highlight(&format_leader_down_highlight(&old_leader, &new_node));
    }

    info!(node_id = state.node_id, conninfo = %target_conninfo, "Handling standby re-point request");
    state
        .supervisor
        .repoint_primary(&target_conninfo)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Repoint failed: {}", e)
                })),
            )
        })?;

    {
        let mut r = state.role.write().await;
        *r = "standby".to_string();
        let mut cfg = state.config.write().await;
        cfg.primary_conninfo = Some(target_conninfo.clone());
    }

    state
        .record_event(
            "node_joined",
            format!(
                "Node {} re-pointed to {}",
                state.node_id, payload.primary_conninfo
            ),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} re-pointed to {}", state.node_id, payload.primary_conninfo),
        "node_id": state.node_id,
        "role": "standby"
    })))
}

pub async fn handle_restore(
    State(state): State<SidecarState>,
    Json(payload): Json<RestorePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Validate target time before transitioning supervisor status
    if let Some(target) = payload.recovery_target_time.as_deref() {
        let trimmed = target.trim();
        if !trimmed.is_empty() {
            let parsed_dt = chrono::DateTime::parse_from_rfc3339(trimmed)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S").map(|ndt| {
                        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(ndt, chrono::Utc)
                    })
                });
            if let Ok(dt) = parsed_dt {
                let now = chrono::Utc::now();
                if dt > now + chrono::Duration::seconds(10) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!(
                                "Recovery target timestamp ({}) cannot be in the future. Current cluster time is {}.",
                                dt.format("%Y-%m-%d %H:%M:%S UTC"),
                                now.format("%Y-%m-%d %H:%M:%S UTC")
                            )
                        })),
                    ));
                }
            }
        }
    }

    let bm = state.backup_manager.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "BackupManager is not configured on this sidecar node"
            })),
        )
    })?;

    info!(snapshot_id = %payload.snapshot_id, "Fetching snapshot archive from storage");
    let (meta, tar_bytes) = bm.get_basebackup(&payload.snapshot_id).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Failed to fetch snapshot {}: {}", payload.snapshot_id, e)
            })),
        )
    })?;

    // Transition supervisor status to Restoring only after validation and archive retrieval succeed
    state.supervisor.set_status(ProcessStatus::Restoring).await;

    let b_type = match meta.backup_type {
        BackupType::Full => "FULL BACKUP",
        BackupType::Incremental => "INCREMENTAL BACKUP",
    };
    let name = meta.label.as_deref().unwrap_or(&payload.snapshot_id);
    log_highlight(&format_restore_highlight(
        b_type,
        name,
        payload.recovery_target_time.as_deref(),
    ));

    info!(snapshot_id = %payload.snapshot_id, bytes = tar_bytes.len(), "Restoring PostgreSQL data directory");

    {
        let mut r = state.role.write().await;
        *r = "leader".to_string();
        let mut cfg = state.config.write().await;
        cfg.primary_conninfo = None;
    }
    let mut cfg = state.config.read().await.clone();
    cfg.primary_conninfo = None;
    state
        .supervisor
        .restore_from_snapshot(&tar_bytes, &cfg, payload.recovery_target_time.as_deref())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to restore from snapshot: {}", e)
                })),
            )
        })?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Snapshot {} restored and PostgreSQL ready", payload.snapshot_id),
        "snapshot_id": payload.snapshot_id
    })))
}

pub async fn handle_prepare_restore(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    info!(
        node_id = state.node_id,
        "Preparing node for cluster restore; entering Restoring state to pause failover"
    );
    state.supervisor.set_status(ProcessStatus::Restoring).await;
    state
        .record_event(
            "cluster_restore",
            format!(
                "Node {} entered Restoring state for cluster restore",
                state.node_id
            ),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} prepared for restore", state.node_id),
        "node_id": state.node_id,
        "status_str": "restoring"
    })))
}

pub async fn handle_cancel_restore(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    info!(
        node_id = state.node_id,
        "Cancelling restoring state on node; returning to operational state"
    );
    let is_running = state.supervisor.is_running().await;
    let new_status = if is_running {
        ProcessStatus::Running
    } else {
        ProcessStatus::Stopped
    };
    state.supervisor.set_status(new_status).await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} restoring state cancelled", state.node_id),
        "node_id": state.node_id,
        "status_str": if is_running { "running" } else { "stopped" }
    })))
}

pub async fn handle_resync(
    State(state): State<SidecarState>,
    payload: Option<Json<ResyncPayload>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state.supervisor.set_status(ProcessStatus::Restoring).await;

    let configured_conninfo = state.config.read().await.primary_conninfo.clone();
    let mut primary_conninfo = payload
        .and_then(|p| p.primary_conninfo.clone())
        .or(configured_conninfo)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "No primary_conninfo provided or configured on this node"
                })),
            )
        })?;

    if !primary_conninfo.contains("application_name=") {
        primary_conninfo.push_str(&format!(" application_name=pgvisor-node{}", state.node_id));
    }

    info!(%primary_conninfo, "Executing standby re-sync from primary");
    {
        let mut cfg = state.config.write().await;
        cfg.primary_conninfo = Some(primary_conninfo.clone());
    }
    let cfg = state.config.read().await.clone();
    state
        .supervisor
        .resync_from_primary(&primary_conninfo, &cfg)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to re-sync standby: {}", e)
                })),
            )
        })?;

    {
        let mut r = state.role.write().await;
        *r = "standby".to_string();
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": "Standby re-synced successfully and PostgreSQL ready"
    })))
}

/// Safely starts Postgres child process, checking peer nodes for an active leader first.
/// If another node is already operating as the active cluster leader, this node reconfigures
/// and starts as a standby replica, preventing split-brain startup and immediate fencing.
pub async fn start_postgres_safely(
    state: &SidecarState,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Probe peers to discover if an active leader is already operating in the cluster
    let mut peer_leader: Option<(u64, String)> = None;
    if !state.peers.is_empty() {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(secret) = state.cluster_secret.as_deref() {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(
                &pgvisor_core::auth::make_auth_header_value(secret),
            ) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
            .default_headers(headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        for peer in state.peers.iter() {
            let self_tag = format!("node{}", state.node_id);
            if peer.contains(&self_tag) {
                continue;
            }
            let url = format!("{}/control/status", peer.trim_end_matches('/'));
            if let Ok(resp) = client.get(&url).send().await {
                if let Ok(st) = resp.json::<StatusResponse>().await {
                    if st.role == "leader" && (st.status == "running" || st.status == "restoring") {
                        let conninfo = format!(
                            "host=pgvisor-node{} port=5432 user=postgres application_name=pgvisor-node{}",
                            st.node_id, state.node_id
                        );
                        peer_leader = Some((st.node_id, conninfo));
                        break;
                    }
                }
            }
        }
    }

    if let Some((leader_id, conninfo)) = peer_leader {
        info!(
            node_id = state.node_id,
            leader_id,
            "Active cluster leader detected on start. Configuring node as standby replica."
        );
        let _ = state.supervisor.repoint_primary(&conninfo).await;
        {
            let mut r = state.role.write().await;
            *r = "standby".to_string();
            let mut cfg = state.config.write().await;
            cfg.primary_conninfo = Some(conninfo.clone());
        }

        let cfg = state.config.read().await.clone();
        let start_res = match state.supervisor.start(&cfg).await {
            Ok(()) => state.supervisor.wait_ready(cfg.port, 15).await,
            Err(e) => Err(e),
        };

        if let Err(err) = start_res {
            warn!(
                node_id = state.node_id,
                ?err,
                "Standby start failed or timeline diverged; re-syncing from primary via pg_basebackup"
            );
            state
                .supervisor
                .resync_from_primary(&conninfo, &cfg)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": format!("Failed to re-sync standby from primary: {}", e)
                        })),
                    )
                })?;
        }
    } else {
        let cfg = state.config.read().await.clone();
        state.supervisor.start(&cfg).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Start failed: {}", e)
                })),
            )
        })?;

        state
            .supervisor
            .wait_ready(cfg.port, 30)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Postgres not ready after start: {}", e)
                    })),
                )
            })?;
    }

    Ok(())
}

pub async fn handle_start(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let current_status = state.supervisor.status().await;
    if current_status == ProcessStatus::Running {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Node {} is already running", state.node_id)
            })),
        ));
    }

    info!(node_id = state.node_id, "Handling start request");
    start_postgres_safely(&state).await?;

    state
        .record_event(
            "node_up",
            format!("Node {} started successfully", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} started successfully", state.node_id),
        "node_id": state.node_id
    })))
}

pub async fn handle_stop(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let current_status = state.supervisor.status().await;
    if current_status == ProcessStatus::Stopped {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Node {} is already stopped", state.node_id)
            })),
        ));
    }

    info!(node_id = state.node_id, "Handling stop request");
    state.supervisor.stop().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Stop failed: {}", e)
            })),
        )
    })?;

    state
        .record_event(
            "node_down",
            format!("Node {} stopped cleanly", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} stopped cleanly", state.node_id),
        "node_id": state.node_id
    })))
}

pub async fn handle_restart(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    info!(node_id = state.node_id, "Handling restart request");
    state.supervisor.stop().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Restart failed: {}", e)
            })),
        )
    })?;

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    start_postgres_safely(&state).await?;

    state
        .record_event(
            "node_up",
            format!("Node {} restarted successfully", state.node_id),
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} restarted successfully", state.node_id),
        "node_id": state.node_id
    })))
}

/// TTL for cluster-wide backup/restore lock before automatic expiration (15 minutes).
pub const BACKUP_LOCK_TTL_MINUTES: i64 = 15;

/// Acquires the cluster-wide backup/restore lock on the leader sidecar.
///
/// Returns 503 if this node is not the current leader, 409 if the lock is already held.
/// On success returns a `lock_token` UUID that must be presented to release the lock.
/// Automatically expires stale locks held longer than `BACKUP_LOCK_TTL_MINUTES`.
pub async fn handle_acquire_backup_lock(
    State(state): State<SidecarState>,
) -> Result<Json<AcquireBackupLockResponse>, (StatusCode, Json<serde_json::Value>)> {
    let role = state.role.read().await.clone();
    if role != "leader" {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "This node is not the cluster leader; direct backup-lock requests to the leader"
            })),
        ));
    }

    let now = chrono::Utc::now();
    let mut lock_guard = state.backup_lock.write().await;

    if let Some(ref existing) = *lock_guard {
        if now < existing.expires_at {
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "A backup or restore operation is already in progress. Please wait for the current operation to complete.",
                    "expires_at": existing.expires_at.to_rfc3339()
                })),
            ));
        }
        warn!(
            token = %existing.token,
            expired_at = %existing.expires_at,
            "Existing backup lock expired; auto-releasing lock"
        );
        *lock_guard = None;
    }

    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = now + chrono::Duration::minutes(BACKUP_LOCK_TTL_MINUTES);

    *lock_guard = Some(BackupLockInfo {
        token: token.clone(),
        acquired_at: now,
        expires_at,
    });

    info!(%token, %expires_at, "Cluster backup lock acquired on leader");
    Ok(Json(AcquireBackupLockResponse {
        lock_token: token,
        acquired_at: now.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
    }))
}

/// Releases the cluster-wide backup/restore lock on the leader sidecar.
///
/// The caller must present the `lock_token` obtained from `/control/backup-lock/acquire`.
/// A token mismatch is silently ignored (idempotent). This prevents a late-arriving
/// release from a crashed proxy from evicting a newly acquired lock.
pub async fn handle_release_backup_lock(
    State(state): State<SidecarState>,
    Json(payload): Json<ReleaseLockPayload>,
) -> impl IntoResponse {
    let mut lock_guard = state.backup_lock.write().await;
    match lock_guard.as_ref() {
        Some(existing) if existing.token == payload.lock_token => {
            info!(token = %payload.lock_token, "Cluster backup lock released");
            *lock_guard = None;
            Json(serde_json::json!({ "status": "ok", "message": "Backup lock released" }))
        }
        Some(existing) => {
            warn!(
                presented = %payload.lock_token,
                stored = %existing.token,
                "Backup lock release ignored: token mismatch"
            );
            Json(
                serde_json::json!({ "status": "ok", "message": "Token mismatch; lock not released" }),
            )
        }
        None => Json(serde_json::json!({ "status": "ok", "message": "Lock was already free" })),
    }
}
