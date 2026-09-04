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
use tracing::{info, warn};

#[derive(Clone)]
struct SidecarState {
    supervisor: Arc<PostgresSupervisor>,
    config: PostgresConfig,
    backup_manager: Option<Arc<BackupManager>>,
    node_id: u64,
    role: String,
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

#[derive(Serialize)]
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
    Json(StatusResponse {
        node_id: state.node_id,
        role: state.role.clone(),
        status: status_str.to_string(),
        child_pid: state.supervisor.child_pid(),
    })
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

    let control_state = SidecarState {
        supervisor: supervisor.clone(),
        config: config.clone(),
        backup_manager,
        node_id,
        role,
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
        .with_state(control_state);

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
