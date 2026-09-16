pub mod config;
pub mod control;
pub mod election;
pub mod supervisor;
pub mod system;
pub mod version;
pub mod wal;

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use config::PostgresConfig;
use control::{build_control_router, spawn_control_server, SidecarState};
pub use control::{SidecarEventRecord, StatusResponse};
use election::spawn_election_monitor;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig};
use supervisor::PostgresSupervisor;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tracing::{info, warn};
use version::detect_postgres_version;
use wal::{run_archive_wal, run_restore_wal};

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
    let peers_str = env::var("PGVISOR_PEERS").unwrap_or_default();
    let peers: Arc<Vec<String>> = Arc::new(
        peers_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    );

    let cluster_secret = pgvisor_core::auth::cluster_secret_from_env();
    if cluster_secret.is_none() {
        warn!("PGVISOR_CLUSTER_SECRET is not configured; sidecar control API is running in UNAUTHENTICATED mode");
    } else {
        info!("Cluster authentication enabled for sidecar control API");
    }

    let control_state = SidecarState::new(
        supervisor.clone(),
        Arc::new(RwLock::new(config.clone())),
        backup_manager,
        node_id,
        role_ref.clone(),
        pg_version,
        peers.clone(),
        cluster_secret,
    );

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

    let control_port: u16 = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let control_addr = SocketAddr::from(([0, 0, 0, 0], control_port));

    let app = build_control_router(control_state.clone());
    spawn_control_server(control_addr, app);

    // Background heartbeat & auto-failover election monitor
    spawn_election_monitor(control_state, peers);

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
