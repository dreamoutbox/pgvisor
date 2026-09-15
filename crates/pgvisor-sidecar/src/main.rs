pub mod config;
pub mod supervisor;
pub mod system;

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use config::PostgresConfig;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig, BackupType};
use pgvisor_core::{
    extract_node_name, format_become_leader_highlight, format_leader_down_highlight,
    format_restore_highlight, log_highlight,
};
use serde::{Deserialize, Serialize};
use supervisor::{PostgresSupervisor, ProcessStatus};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Auditable event record tracked within local sidecar lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarEventRecord {
    pub id: u64,
    pub timestamp: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone)]
struct SidecarState {
    supervisor: Arc<PostgresSupervisor>,
    config: Arc<RwLock<PostgresConfig>>,
    backup_manager: Option<Arc<BackupManager>>,
    node_id: u64,
    role: Arc<RwLock<String>>,
    pg_version: String,
    events: Arc<RwLock<VecDeque<SidecarEventRecord>>>,
    event_id: Arc<AtomicU64>,
    system_metrics: Arc<system::SystemMetricsCollector>,
    peers: Arc<Vec<String>>,
}

impl SidecarState {
    async fn record_event(&self, kind: &str, detail: impl Into<String>) {
        let id = self.event_id.fetch_add(1, Ordering::SeqCst);
        let record = SidecarEventRecord {
            id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind: kind.to_string(),
            detail: detail.into(),
        };
        let mut q = self.events.write().await;
        if q.len() >= 200 {
            q.pop_front();
        }
        q.push_back(record);
    }
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
pub struct StatusResponse {
    pub node_id: u64,
    pub role: String,
    pub status: String,
    pub child_pid: u32,
    pub pg_version: String,
    pub uptime_secs: u64,
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
}

async fn handle_status(State(state): State<SidecarState>) -> impl IntoResponse {
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

async fn handle_promote(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
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
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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

async fn handle_demote(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    info!(node_id = state.node_id, "Handling demotion request");
    state.supervisor.stop().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Demotion failed: {}", e)
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

async fn handle_repoint(
    State(state): State<SidecarState>,
    Json(payload): Json<RepointPayload>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
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
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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
    let (meta, tar_bytes) = bm.get_basebackup(&payload.snapshot_id).await.map_err(|e| {
        (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Failed to fetch snapshot {}: {}", payload.snapshot_id, e)
            })),
        )
    })?;

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
    let configured_conninfo = state.config.read().await.primary_conninfo.clone();
    let mut primary_conninfo = payload
        .and_then(|p| p.primary_conninfo.clone())
        .or(configured_conninfo)
        .ok_or_else(|| {
            (
                axum::http::StatusCode::BAD_REQUEST,
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
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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
async fn start_postgres_safely(
    state: &SidecarState,
) -> Result<(), (axum::http::StatusCode, Json<serde_json::Value>)> {
    // Probe peers to discover if an active leader is already operating in the cluster
    let mut peer_leader: Option<(u64, String)> = None;
    if !state.peers.is_empty() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
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
                    if st.role == "leader" && st.status == "running" {
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
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Postgres not ready after start: {}", e)
                    })),
                )
            })?;
    }

    Ok(())
}

async fn handle_start(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let current_status = state.supervisor.status().await;
    if current_status == ProcessStatus::Running {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
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

async fn handle_stop(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let current_status = state.supervisor.status().await;
    if current_status == ProcessStatus::Stopped {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Node {} is already stopped", state.node_id)
            })),
        ));
    }

    info!(node_id = state.node_id, "Handling stop request");
    state.supervisor.stop().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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

async fn handle_restart(
    State(state): State<SidecarState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    info!(node_id = state.node_id, "Handling restart request");
    state.supervisor.stop().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
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

async fn detect_postgres_version(data_dir: &std::path::Path) -> String {
    if let Ok(output) = tokio::process::Command::new("postgres")
        .arg("-V")
        .output()
        .await
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for part in stdout.split_whitespace() {
                let trimmed = part.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
                if trimmed.contains('.') && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.')
                {
                    return trimmed.to_string();
                }
            }
        }
    }
    let pg_version_file = data_dir.join("PG_VERSION");
    if let Ok(content) = tokio::fs::read_to_string(pg_version_file).await {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    "18.6".to_string()
}

async fn run_archive_wal(source_path: &str, file_name: &str) -> Result<()> {
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

async fn run_restore_wal(file_name: &str, target_path: &str) -> Result<()> {
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

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && (args[1] == "archive" || args[1] == "archive-wal") {
        if args.len() < 4 {
            eprintln!("Usage: pgvisor-sidecar archive <source_path> <file_name>");
            std::process::exit(1);
        }
        return run_archive_wal(&args[2], &args[3]).await;
    }
    if args.len() > 1 && (args[1] == "restore" || args[1] == "restore-wal") {
        if args.len() < 4 {
            eprintln!("Usage: pgvisor-sidecar restore <file_name> <target_path>");
            std::process::exit(1);
        }
        return run_restore_wal(&args[2], &args[3]).await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
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
    let cluster_id =
        env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());

    let superuser = env::var("POSTGRES_USER").unwrap_or_else(|_| "postgres".to_string());
    let mut primary_conninfo = env::var("PRIMARY_CONNINFO").ok();
    if let Some(conn) = primary_conninfo.as_mut() {
        if !conn.contains("application_name=") {
            conn.push_str(&format!(" application_name=pgvisor-node{}", node_id));
        }
    }

    info!(?data_dir, port, %superuser, node_id, %role, "pgvisor-sidecar supervisor starting up");

    let supervisor = Arc::new(PostgresSupervisor::new(&data_dir));
    supervisor
        .ensure_initialized(&superuser, primary_conninfo.as_deref())
        .await?;

    let sidecar_bin = env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "pgvisor-sidecar".to_string());

    let archive_cmd = format!("{sidecar_bin} archive %p %f");
    let restore_cmd = format!("{sidecar_bin} restore %f %p");

    let config = PostgresConfig {
        port,
        primary_conninfo,
        archive_command: Some(archive_cmd),
        restore_command: Some(restore_cmd),
        ..Default::default()
    };

    supervisor.start(&config).await?;

    let pg_version = detect_postgres_version(&data_dir).await;
    info!(%pg_version, "Detected PostgreSQL server version");

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

    let role_ref = Arc::new(RwLock::new(role.clone()));
    let events_queue = Arc::new(RwLock::new(VecDeque::new()));
    let event_id_counter = Arc::new(AtomicU64::new(1));
    let system_metrics = Arc::new(system::SystemMetricsCollector::new());

    let peers_str = env::var("PGVISOR_PEERS").unwrap_or_default();
    let peers: Arc<Vec<String>> = Arc::new(
        peers_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    );

    let control_state = SidecarState {
        supervisor: supervisor.clone(),
        config: Arc::new(RwLock::new(config.clone())),
        backup_manager,
        node_id,
        role: role_ref.clone(),
        pg_version,
        events: events_queue.clone(),
        event_id: event_id_counter.clone(),
        system_metrics,
        peers: peers.clone(),
    };

    // Log initial startup event
    if role == "leader" {
        control_state
            .record_event(
                "election_result",
                format!("Node {} initialized as cluster leader", node_id),
            )
            .await;
    } else {
        control_state
            .record_event(
                "node_joined",
                format!("Node {} joined cluster as standby replica", node_id),
            )
            .await;
    }

    #[derive(Deserialize)]
    struct EventsQuery {
        since_id: Option<u64>,
    }

    async fn handle_events(
        State(state): State<SidecarState>,
        Query(query): Query<EventsQuery>,
    ) -> Json<Vec<SidecarEventRecord>> {
        let since_id = query.since_id.unwrap_or(0);
        let q = state.events.read().await;
        let records: Vec<SidecarEventRecord> =
            q.iter().filter(|e| e.id > since_id).cloned().collect();
        Json(records)
    }

    let control_port: u16 = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let control_addr = SocketAddr::from(([0, 0, 0, 0], control_port));

    let app = Router::new()
        .route("/control/status", get(handle_status))
        .route("/control/events", get(handle_events))
        .route("/control/start", post(handle_start))
        .route("/control/stop", post(handle_stop))
        .route("/control/restart", post(handle_restart))
        .route("/control/restore", post(handle_restore))
        .route("/control/resync", post(handle_resync))
        .route("/control/promote", post(handle_promote))
        .route("/control/fence", post(handle_fence))
        .route("/control/demote", post(handle_demote))
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
    if !peers.is_empty() {
        let peers = peers.clone();
        let monitor_state = control_state.clone();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        tokio::spawn(async move {
            let mut missed_heartbeats = 0u32;
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

            loop {
                interval.tick().await;

                let local_status = monitor_state.supervisor.status().await;
                if local_status == ProcessStatus::Restoring
                    || local_status == ProcessStatus::Stopped
                {
                    // Node is actively restoring, re-syncing, or intentionally stopped; pause auto-failover actions
                    missed_heartbeats = 0;
                    continue;
                }

                let local_role = monitor_state.role.read().await.clone();
                if local_role == "fenced" {
                    // Check if an active leader is operating and available for auto-rejoin
                    let mut active_leader: Option<(u64, String)> = None;

                    for peer in peers.iter() {
                        let self_tag = format!("node{}", monitor_state.node_id);
                        if peer.contains(&self_tag) {
                            continue;
                        }
                        let url = format!("{}/control/status", peer.trim_end_matches('/'));
                        if let Ok(resp) = client.get(&url).send().await {
                            if let Ok(st) = resp.json::<StatusResponse>().await {
                                if st.role == "leader" && st.status == "running" {
                                    let conninfo = format!(
                                        "host=pgvisor-node{} port=5432 user=postgres application_name=pgvisor-node{}",
                                        st.node_id, monitor_state.node_id
                                    );
                                    active_leader = Some((st.node_id, conninfo));
                                    break;
                                }
                            }
                        }
                    }

                    if let Some((leader_id, conninfo)) = active_leader {
                        info!(
                            node_id = monitor_state.node_id,
                            leader_id,
                            "Fenced node detected active cluster leader. Initiating auto-rejoin as standby replica."
                        );

                        let mut standby_config = monitor_state.config.read().await.clone();
                        standby_config.primary_conninfo = Some(conninfo.clone());

                        match monitor_state
                            .supervisor
                            .resync_from_primary(&conninfo, &standby_config)
                            .await
                        {
                            Ok(()) => {
                                info!(
                                    node_id = monitor_state.node_id,
                                    leader_id,
                                    "Successfully auto-rejoined cluster as standby replica."
                                );
                                {
                                    let mut r = monitor_state.role.write().await;
                                    *r = "standby".to_string();
                                    let mut cfg = monitor_state.config.write().await;
                                    cfg.primary_conninfo = Some(conninfo.clone());
                                }
                                monitor_state
                                    .record_event(
                                        "node_joined",
                                        format!(
                                            "Node {} auto-rejoined cluster as standby under leader {}",
                                            monitor_state.node_id, leader_id
                                        ),
                                    )
                                    .await;
                                missed_heartbeats = 0;
                            }
                            Err(e) => {
                                error!(
                                    node_id = monitor_state.node_id,
                                    leader_id,
                                    ?e,
                                    "Failed to auto-rejoin as standby; will retry on next cycle"
                                );
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            }
                        }
                    }
                    continue;
                }

                if local_role == "leader" {
                    // Split-brain guard: check if another peer is already operating as active leader
                    for peer in peers.iter() {
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
                                    monitor_state
                                        .record_event(
                                            "election_result",
                                            format!(
                                                "Node {} fenced due to split-brain leader detection",
                                                monitor_state.node_id
                                            ),
                                        )
                                        .await;
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

                for peer in peers.iter() {
                    let url = format!("{}/control/status", peer.trim_end_matches('/'));
                    if let Ok(resp) = client.get(&url).send().await {
                        if let Ok(st) = resp.json::<StatusResponse>().await {
                            if st.status == "running" || st.status == "restoring" {
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

                    // Election timeout: 5 consecutive misses (2500ms) matches Ticket 004
                    if missed_heartbeats >= 5 {
                        let quorum = (peers.len() / 2) + 1;
                        if alive_nodes.len() >= quorum {
                            if let Some(&winner_id) = alive_nodes.first() {
                                if winner_id == monitor_state.node_id {
                                    let old_conninfo = monitor_state
                                        .config
                                        .read()
                                        .await
                                        .primary_conninfo
                                        .clone()
                                        .unwrap_or_default();
                                    let old_node = extract_node_name(&old_conninfo);
                                    let old_leader = if old_node == "unknown"
                                        || old_node.is_empty()
                                        || old_node == format!("node{}", monitor_state.node_id)
                                    {
                                        "node1".to_string()
                                    } else {
                                        old_node
                                    };
                                    let my_node = format!("node{}", monitor_state.node_id);
                                    log_highlight(&format_become_leader_highlight(
                                        &old_leader,
                                        &my_node,
                                    ));

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
                                            let mut cfg = monitor_state.config.write().await;
                                            cfg.primary_conninfo = None;
                                        }
                                        monitor_state
                                            .record_event(
                                                "election_result",
                                                format!(
                                                    "Node {} auto-promoted to leader after quorum election",
                                                    monitor_state.node_id
                                                ),
                                            )
                                            .await;
                                        missed_heartbeats = 0;

                                        // Broadcast repoint to peer standbys (excluding self)
                                        let my_conninfo = format!(
                                            "host=pgvisor-node{} port=5432 user=postgres",
                                            monitor_state.node_id
                                        );
                                        let self_host = format!("node{}:", monitor_state.node_id);
                                        let self_host2 = format!("node{}", monitor_state.node_id);
                                        for peer in peers.iter() {
                                            if peer.contains(&self_host)
                                                || peer.ends_with(&self_host2)
                                            {
                                                continue;
                                            }
                                            let repoint_url = format!(
                                                "{}/control/repoint",
                                                peer.trim_end_matches('/')
                                            );
                                            let payload = serde_json::json!({
                                                "primary_conninfo": my_conninfo
                                            });
                                            let _ = client
                                                .post(&repoint_url)
                                                .json(&payload)
                                                .send()
                                                .await;
                                        }
                                    }
                                } else {
                                    info!(
                                        node_id = monitor_state.node_id,
                                        winner_id, "Waiting for peer candidate to promote"
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_sidecar_cli_args_parsing() {
        let archive_args = vec![
            "pgvisor-sidecar".to_string(),
            "archive".to_string(),
            "pg_wal/000000010000000000000001".to_string(),
            "000000010000000000000001".to_string(),
        ];
        assert!(archive_args[1] == "archive" || archive_args[1] == "archive-wal");
        assert_eq!(archive_args[2], "pg_wal/000000010000000000000001");
        assert_eq!(archive_args[3], "000000010000000000000001");

        let restore_args = vec![
            "pgvisor-sidecar".to_string(),
            "restore".to_string(),
            "000000010000000000000001".to_string(),
            "pg_wal/RECOVERYXLOG".to_string(),
        ];
        assert!(restore_args[1] == "restore" || restore_args[1] == "restore-wal");
        assert_eq!(restore_args[2], "000000010000000000000001");
        assert_eq!(restore_args[3], "pg_wal/RECOVERYXLOG");
    }
}
