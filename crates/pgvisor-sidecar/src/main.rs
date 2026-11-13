pub mod api;
pub mod config;
pub mod control;
pub mod election;
pub mod logging;
pub mod scheduler;
pub mod supervisor;
pub mod system;
pub mod version;
pub mod wal;

use std::env;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use api::{
    build_api_router, resolve_sidecar_ports, spawn_api_server, ApiState, BackupRetentionConfig,
    SessionTokenSigner, SuperuserAllowlist,
};
use config::{LogStatement, PostgresConfig};
use control::{build_control_router, spawn_control_server, SidecarState};
pub use control::{SidecarEventRecord, StatusResponse};
use election::spawn_election_monitor;
use logging::SidecarLogCaptureLayer;
use pgvisor_core::audit::AuditLog;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig};
use pgvisor_core::pg_client::PgConnection;
use supervisor::{InitOutcome, PostgresSupervisor};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::RwLock;
use tracing::{info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
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

    let log_buffer = Arc::new(std::sync::RwLock::new(std::collections::VecDeque::with_capacity(
        supervisor::MAX_LOG_ENTRIES,
    )));

    let capture_layer = SidecarLogCaptureLayer::new(Arc::clone(&log_buffer));
    let fmt_layer = tracing_subscriber::fmt::layer().with_ansi(false);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(capture_layer)
        .init();

    let data_dir = env::var("PGDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/postgresql/data"));

    let port: u16 = env::var("PGPORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5432);

    let control_port: u16 = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let api_port: u16 = env::var("PGVISOR_API_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8081);

    resolve_sidecar_ports(control_port, api_port)?;

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

    let supervisor = Arc::new(PostgresSupervisor::with_log_buffer(&data_dir, log_buffer));
    let init_outcome = supervisor
        .ensure_initialized(&superuser, primary_conninfo.as_deref())
        .await?;

    let sidecar_bin = env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "pgvisor-sidecar".to_string());

    let archive_cmd = format!("{sidecar_bin} archive %p %f");
    let restore_cmd = format!("{sidecar_bin} restore %f %p");

    let tls_enabled = env::var("PGVISOR_TLS_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    let (ssl, ssl_cert_file, ssl_key_file) = if tls_enabled {
        let cert_path = env::var("PGVISOR_TLS_CERT_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("server.crt"));
        let key_path = env::var("PGVISOR_TLS_KEY_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("server.key"));

        let node_host = format!("pgvisor-node{}", node_id);
        let alt_names = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            node_host.clone(),
        ];

        let pair = pgvisor_core::tls::TlsCertPair::load_or_generate(
            &cert_path, &key_path, &node_host, &alt_names,
        )?;

        // If custom external certificates/keys are provided (e.g. mounted from host),
        // PostgreSQL strictly requires the private key file to be owned by the postgres database user
        // and have mode 0600. Host bind mounts retain host ownership (e.g. UID 1000),
        // which causes PostgreSQL to fail startup.
        // Therefore, stage the key and cert into data_dir owned by postgres with 0600 mode.
        let default_cert = data_dir.join("server.crt");
        let default_key = data_dir.join("server.key");
        let (active_cert_path, active_key_path) = if key_path != default_key {
            std::fs::write(&default_cert, &pair.cert_pem)?;
            std::fs::set_permissions(&default_cert, std::fs::Permissions::from_mode(0o644))?;

            std::fs::write(&default_key, &pair.key_pem)?;
            std::fs::set_permissions(&default_key, std::fs::Permissions::from_mode(0o600))?;

            info!(
                source_cert = ?cert_path,
                source_key = ?key_path,
                staged_cert = ?default_cert,
                staged_key = ?default_key,
                "Staged TLS certificate and private key into data_dir with 0600 permissions"
            );
            (default_cert, default_key)
        } else {
            (cert_path, key_path)
        };

        if let Some(conn) = primary_conninfo.as_mut() {
            if !conn.contains("sslmode=") {
                conn.push_str(" sslmode=prefer");
            }
        }

        info!(
            cert_path = ?active_cert_path,
            key_path = ?active_key_path,
            "TLS enabled and certificates initialized for PostgreSQL"
        );
        (
            true,
            Some(active_cert_path.to_string_lossy().to_string()),
            Some(active_key_path.to_string_lossy().to_string()),
        )
    } else {
        (false, None, None)
    };

    let log_statement = env::var("PGVISOR_DEBUG_LOG_STATEMENT")
        .ok()
        .and_then(|v| LogStatement::parse(&v))
        .unwrap_or(LogStatement::Mod);

    let config = PostgresConfig {
        port,
        primary_conninfo,
        archive_command: Some(archive_cmd),
        restore_command: Some(restore_cmd),
        ssl,
        ssl_cert_file,
        ssl_key_file,
        log_statement,
        ..Default::default()
    };

    supervisor.start(&config).await?;

    let pg_addr = format!("127.0.0.1:{port}");
    if role == "leader" {
        if let Ok(pg_password) = env::var("POSTGRES_PASSWORD") {
            if !pg_password.is_empty() {
                bootstrap_superuser_password(&pg_addr, &superuser, &pg_password, init_outcome)
                    .await;
            }
        }
    }

    let pg_version = detect_postgres_version(&data_dir).await;
    info!(%pg_version, "Detected PostgreSQL server version");

    // Initialize OpenDAL storage operator for backup management if configured
    let mut backup_config = BackupScheduleConfig::from_env();
    if env::var("S3_ENDPOINT").is_err() && env::var("PGVISOR_S3_ENDPOINT").is_err() {
        backup_config.minio_endpoint = "http://minio:9000".to_string();
    }
    let backup_retention = BackupRetentionConfig::from_schedule_config(&backup_config);

    let s3_operator = backup_config.build_operator().ok();
    let backup_manager = s3_operator
        .clone()
        .map(|operator| Arc::new(BackupManager::new(&cluster_id, operator)));
    if backup_manager.is_none() {
        warn!("OpenDAL operator not initialized in sidecar");
    }

    let audit_log = Arc::new(AuditLog::new(&cluster_id, s3_operator, 2000));
    let audit_loader = Arc::clone(&audit_log);
    tokio::spawn(async move {
        match audit_loader.load_from_storage().await {
            Ok(count) => info!(count, "Restored historical audit log events from S3 storage"),
            Err(e) => warn!(?e, "Could not restore audit logs from S3"),
        }
    });

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
        cluster_secret.clone(),
    )
    .with_api_port(api_port)
    .with_pg_endpoint(port, &superuser);

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

    let control_addr = SocketAddr::from(([0, 0, 0, 0], control_port));
    let control_app = build_control_router(control_state.clone());
    spawn_control_server(control_addr, control_app);

    let superusers = Arc::new(SuperuserAllowlist::from_env(&superuser));
    let token_signer = Arc::new(SessionTokenSigner::from_cluster_secret(
        cluster_secret.as_deref(),
    ));
    let mut api_state = ApiState::new(
        control_state.clone(),
        pg_addr,
        superuser,
        superusers,
        token_signer,
        audit_log,
    )
    .with_cluster_id(&cluster_id)
    .with_backup_retention(backup_retention);
    if env::var("PGVISOR_DASHBOARD_ALLOW_MUTATIONS")
        .map(|v| v.eq_ignore_ascii_case("false") || v == "0")
        .unwrap_or(false)
    {
        api_state = api_state.with_security_guard(Arc::new(
            pgvisor_core::sql_guard::SqlSecurityGuard::new_read_only(
                std::time::Duration::from_secs(10),
                500,
            ),
        ));
    }
    api::spawn_metrics_sampler(api_state.clone());
    Arc::new(scheduler::BackupScheduler::new(
        api_state.clone(),
        backup_config,
    ))
    .start();
    let api_addr = SocketAddr::from(([0, 0, 0, 0], api_port));
    let api_app = build_api_router(api_state);
    spawn_api_server(api_addr, api_app);

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

/// Sets the PostgreSQL superuser password from `POSTGRES_PASSWORD` after `initdb`
/// or when no password verifier is currently stored on the leader.
async fn bootstrap_superuser_password(
    pg_addr: &str,
    superuser: &str,
    password: &str,
    init_outcome: InitOutcome,
) {
    let escaped_role = superuser.replace('"', "\"\"");
    let escaped_lit = superuser.replace('\'', "''");

    let should_set = if init_outcome == InitOutcome::InitializedPrimary {
        true
    } else {
        let check_sql = format!(
            "SELECT COALESCE(rolpassword, '') FROM pg_authid WHERE rolname = '{escaped_lit}' LIMIT 1"
        );
        match PgConnection::query_simple(pg_addr, Some(superuser), Some("postgres"), &check_sql, 1)
            .await
        {
            Ok(res) => res
                .rows
                .first()
                .and_then(|r| r.first())
                .map(|pw| pw.is_empty())
                .unwrap_or(false),
            Err(e) => {
                warn!(?e, "Failed to check existing PostgreSQL superuser password");
                false
            }
        }
    };

    if !should_set {
        return;
    }

    let escaped_pw = password.replace('\'', "''");
    let alter_sql = format!("ALTER USER \"{escaped_role}\" WITH PASSWORD '{escaped_pw}'");
    match PgConnection::query_simple(pg_addr, Some(superuser), Some("postgres"), &alter_sql, 1).await
    {
        Ok(_) => info!(%superuser, "Bootstrapped PostgreSQL superuser password from POSTGRES_PASSWORD"),
        Err(e) => warn!(?e, %superuser, "Failed to set PostgreSQL superuser password"),
    }
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
