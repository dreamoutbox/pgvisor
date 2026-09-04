use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use pgvisor_dashboard::handlers::DashboardState;
use pgvisor_dashboard::create_router;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let bind_addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let admin_token = std::env::var("PGVISOR_ADMIN_TOKEN").ok();
    let cluster_id = std::env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-primary".into());

    let state = Arc::new(DashboardState::new(&cluster_id, admin_token));
    let app = create_router(state);

    info!(addr = %bind_addr, "PgVisor dashboard HTTP server listening");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
