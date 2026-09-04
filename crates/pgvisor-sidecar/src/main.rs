pub mod config;
pub mod supervisor;

use std::env;
use std::path::PathBuf;

use anyhow::Result;
use config::PostgresConfig;
use supervisor::PostgresSupervisor;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn};

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

    let superuser = env::var("POSTGRES_USER").unwrap_or_else(|_| "postgres".to_string());
    let primary_conninfo = env::var("PRIMARY_CONNINFO").ok();

    info!(?data_dir, port, %superuser, "pgvisor-sidecar supervisor starting up");

    let supervisor = PostgresSupervisor::new(&data_dir);
    supervisor
        .ensure_initialized(&superuser, primary_conninfo.as_deref())
        .await?;

    let config = PostgresConfig {
        port,
        primary_conninfo,
        ..Default::default()
    };

    supervisor.start(&config).await?;

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
