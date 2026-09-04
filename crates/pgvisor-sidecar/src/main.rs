pub mod config;
pub mod supervisor;

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use config::PostgresConfig;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig};
use serde::{Deserialize, Serialize};
use supervisor::{PostgresSupervisor, ProcessStatus};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

#[derive(Clone)]
struct SidecarState {
    supervisor: Arc<PostgresSupervisor>,
    config: PostgresConfig,
    backup_manager: Option<Arc<BackupManager>>,
    node_id: u64,
    role: Arc<RwLock<String>>,
}

#[derive(Deserialize)]
struct RestorePayload {
    snapshot_id: String,
    recovery_target_time: Option<String>,
}

#[derive(Deserialize, Default)]
struct ResyncPayload {
    primary_conninfo: Option<String>,
}

#[derive(Deserialize)]
struct RepointPayload {
    primary_conninfo: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct StatusResponse {
    node_id: u64,
    role: String,
    status: String,
    child_pid: u32,
}

async fn handle_status(State(state): State<SidecarState>) -> impl IntoResponse {
    let supervisor_status = state.supervisor.status().await;
    let status_str = match supervisor_status {
        ProcessStatus::Running => "running",
        ProcessStatus::Stopped => "stopped",
        ProcessStatus::Fenced => "fenced",
    };
    let current_role = state.role.read().await.clone();
    Json(StatusResponse {
        node_id: state.node_id,
        role: current_role,
        status: status_str.to_string(),
        child_pid: state.supervisor.child_pid(),
    })
}

async fn handle_promote(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    info!(node_id = state.node_id, "Handling manual/automated promotion request");
    state.supervisor.promote().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Promotion failed: {}", e)
            })),
        )
    })?;

    {
        let mut r = state.role.write().await;
        *r = "leader".to_string();
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} successfully promoted to leader", state.node_id),
        "node_id": state.node_id,
        "role": "leader"
    })))
}

async fn handle_fence(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    warn!(node_id = state.node_id, "Handling fencing request");
    state.supervisor.fence().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Fencing failed: {}", e)
            })),
        )
    })?;

    {
        let mut r = state.role.write().await;
        *r = "fenced".to_string();
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} fenced immediately", state.node_id),
        "node_id": state.node_id,
        "role": "fenced"
    })))
}

async fn handle_repoint(
    State(state): State<SidecarState>,
    Json(payload): Json<RepointPayload>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let current_role = state.role.read().await.clone();
    if current_role == "leader" {
        info!(node_id = state.node_id, "Ignoring repoint request because node is currently leader");
        return Ok(Json(serde_json::json!({
            "status": "ok",
            "message": "Node is leader, ignoring repoint",
            "node_id": state.node_id,
            "role": "leader"
        })));
    }

    info!(node_id = state.node_id, conninfo = %payload.primary_conninfo, "Handling standby re-point request");
    state.supervisor.repoint_primary(&payload.primary_conninfo).await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Repoint failed: {}", e)
            })),
        )
    })?;

    {
        let mut r = state.role.write().await;
        *r = "standby".to_string();
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node {} re-pointed to {}", state.node_id, payload.primary_conninfo),
        "node_id": state.node_id,
        "role": "standby"
    })))
}

async fn handle_restore(
    State(state): State<SidecarState>,
    Json(payload): Json<RestorePayload>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let bm = state.backup_manager.as_ref().ok_or_else(|| {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "BackupManager is not configured on this sidecar node"
            })),
        )
    })?;

    info!(snapshot_id = %payload.snapshot_id, "Fetching snapshot archive from storage");
    let (_meta, tar_bytes) = bm
        .get_basebackup(&payload.snapshot_id)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Failed to fetch snapshot {}: {}", payload.snapshot_id, e)
                })),
            )
        })?;

    info!(snapshot_id = %payload.snapshot_id, bytes = tar_bytes.len(), "Restoring PostgreSQL data directory");
    state
        .supervisor
        .restore_from_snapshot(
            &tar_bytes,
            &state.config,
            payload.recovery_target_time.as_deref(),
        )
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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

async fn handle_resync(
    State(state): State<SidecarState>,
    payload: Option<Json<ResyncPayload>>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let primary_conninfo = payload
        .and_then(|p| p.primary_conninfo.clone())
        .or_else(|| state.config.primary_conninfo.clone())
        .ok_or_else(|| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "No primary_conninfo provided or configured on this node"
                })),
            )
        })?;

    info!(%primary_conninfo, "Executing standby re-sync from primary");
    state
        .supervisor
        .resync_from_primary(&primary_conninfo, &state.config)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to re-sync standby: {}", e)
                })),
            )
        })?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": "Standby re-synced successfully and PostgreSQL ready"
    })))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let data_dir = env::var("PGDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/postgresql/data"));

    let port: u16 = env::var("PGPORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5432);

    let node_id: u64 = env::var("PGVISOR_NODE_ID")
        .ok()
        .and_then(|id| id.parse().ok())
        .unwrap_or(1);

    let role = env::var("PGVISOR_ROLE").unwrap_or_else(|_| "leader".to_string());
    let cluster_id = env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());

    let superuser = env::var("POSTGRES_USER").unwrap_or_else(|_| "postgres".to_string());
    let primary_conninfo = env::var("PRIMARY_CONNINFO").ok();

    info!(?data_dir, port, %superuser, node_id, %role, "pgvisor-sidecar supervisor starting up");

    let supervisor = Arc::new(PostgresSupervisor::new(&data_dir));
    supervisor
        .ensure_initialized(&superuser, primary_conninfo.as_deref())
        .await?;

    let config = PostgresConfig {
        port,
        primary_conninfo,
        ..Default::default()
    };

    supervisor.start(&config).await?;

    // Initialize OpenDAL storage operator for backup management if configured
    let s3_endpoint = env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
    let s3_bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "pgvisor-backups".to_string());
    let s3_access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let s3_secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());

    let backup_config = BackupScheduleConfig {
        minio_endpoint: s3_endpoint,
        minio_bucket: s3_bucket,
        access_key: s3_access_key,
        secret_key: s3_secret_key,
        ..Default::default()
    };

    let backup_manager = match backup_config.build_operator() {
        Ok(operator) => Some(Arc::new(BackupManager::new(&cluster_id, operator))),
        Err(e) => {
            warn!(?e, "OpenDAL operator not initialized in sidecar");
            None
        }
    };

    let role_ref = Arc::new(RwLock::new(role));

    let control_state = SidecarState {
        supervisor: supervisor.clone(),
        config: config.clone(),
        backup_manager,
        node_id,
        role: role_ref.clone(),
    };

    let control_port: u16 = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let control_addr = SocketAddr::from(([0, 0, 0, 0], control_port));

    let app = Router::new()
        .route("/control/status", get(handle_status))
        .route("/control/restore", post(handle_restore))
        .route("/control/resync", post(handle_resync))
        .route("/control/promote", post(handle_promote))
        .route("/control/fence", post(handle_fence))
        .route("/control/repoint", post(handle_repoint))
        .with_state(control_state.clone());

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(control_addr).await {
            Ok(listener) => {
                info!(addr = %control_addr, "pgvisor-sidecar control server listening");
                let _ = axum::serve(listener, app).await;
            }
            Err(e) => {
                warn!(addr = %control_addr, ?e, "Failed to bind sidecar control server");
            }
        }
    });

    // Background heartbeat & auto-failover election monitor
    let peers_str = env::var("PGVISOR_PEERS").unwrap_or_default();
    let peers: Vec<String> = peers_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if !peers.is_empty() {
        let monitor_state = control_state.clone();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        tokio::spawn(async move {
            let mut missed_heartbeats = 0u32;
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

            loop {
                interval.tick().await;

                let local_role = monitor_state.role.read().await.clone();
                if local_role == "fenced" {
                    continue;
                }

                if local_role == "leader" {
                    // Split-brain guard: check if another peer is already operating as active leader
                    for peer in &peers {
                        let self_tag = format!("node{}", monitor_state.node_id);
                        if peer.contains(&self_tag) {
                            continue;
                        }
                        let url = format!("{}/control/status", peer.trim_end_matches('/'));
                        if let Ok(resp) = client.get(&url).send().await {
                            if let Ok(st) = resp.json::<StatusResponse>().await {
                                if st.role == "leader" && st.status == "running" {
                                    warn!(
                                        node_id = monitor_state.node_id,
                                        peer_leader = st.node_id,
                                        "Split-brain detected: another node is already active leader! Fencing local instance to prevent data corruption."
                                    );
                                    let _ = monitor_state.supervisor.fence().await;
                                    {
                                        let mut r = monitor_state.role.write().await;
                                        *r = "fenced".to_string();
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    continue;
                }

                // Standby mode: probe peers for active leader
                let mut leader_found = false;
                let mut alive_nodes: Vec<u64> = vec![monitor_state.node_id];

                for peer in &peers {
                    let url = format!("{}/control/status", peer.trim_end_matches('/'));
                    if let Ok(resp) = client.get(&url).send().await {
                        if let Ok(st) = resp.json::<StatusResponse>().await {
                            if st.status == "running" {
                                alive_nodes.push(st.node_id);
                                if st.role == "leader" {
                                    leader_found = true;
                                }
                            }
                        }
                    }
                }

                alive_nodes.sort_unstable();
                alive_nodes.dedup();

                if leader_found {
                    missed_heartbeats = 0;
                } else {
                    missed_heartbeats += 1;
                    info!(
                        node_id = monitor_state.node_id,
                        missed_heartbeats,
                        alive_count = alive_nodes.len(),
                        "No active leader detected"
                    );

                    // Election timeout: 3 consecutive misses (1500ms) matches Ticket 004
                    if missed_heartbeats >= 3 {
                        let quorum = (peers.len() / 2) + 1;
                        if alive_nodes.len() >= quorum {
                            if let Some(&winner_id) = alive_nodes.first() {
                                if winner_id == monitor_state.node_id {
                                    info!(
                                        node_id = monitor_state.node_id,
                                        "Quorum achieved and candidate ID matches: PROMOTING TO LEADER"
                                    );
                                    if let Err(e) = monitor_state.supervisor.promote().await {
                                        error!(?e, "Failed to promote PostgreSQL to leader");
                                    } else {
                                        {
                                            let mut r = monitor_state.role.write().await;
                                            *r = "leader".to_string();
                                        }
                                        missed_heartbeats = 0;

                                        // Broadcast repoint to peer standbys (excluding self)
                                        let my_conninfo = format!(
                                            "host=pgvisor-node{} port=5432 user=postgres",
                                            monitor_state.node_id
                                        );
                                        let self_host = format!("node{}:", monitor_state.node_id);
                                        let self_host2 = format!("node{}", monitor_state.node_id);
                                        for peer in &peers {
                                            if peer.contains(&self_host) || peer.ends_with(&self_host2) {
                                                continue;
                                            }
                                            let repoint_url =
                                                format!("{}/control/repoint", peer.trim_end_matches('/'));
                                            let payload = serde_json::json!({
                                                "primary_conninfo": my_conninfo
                                            });
                                            let _ = client.post(&repoint_url).json(&payload).send().await;
                                        }
                                    }
                                } else {
                                    info!(
                                        node_id = monitor_state.node_id,
                                        winner_id,
                                        "Waiting for peer candidate to promote"
                                    );
                                }
                            }
                        } else {
                            warn!(
                                node_id = monitor_state.node_id,
                                alive = alive_nodes.len(),
                                required = quorum,
                                "Quorum lost; cannot elect new leader"
                            );
                        }
                    }
                }
            }
        });
    }

    // Setup container PID 1 signal listeners
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigquit = signal(SignalKind::quit())?;

    tokio::select! {
        _ = sigterm.recv() => {
            info!("Received SIGTERM, initiating graceful fast shutdown of Postgres");
            supervisor.stop().await?;
        }
        _ = sigint.recv() => {
            info!("Received SIGINT, initiating graceful shutdown of Postgres");
            supervisor.stop().await?;
        }
        _ = sigquit.recv() => {
            warn!("Received SIGQUIT, triggering immediate fencing of Postgres");
            supervisor.fence().await?;
        }
    }

    info!("pgvisor-sidecar stopped cleanly");
    Ok(())
}
