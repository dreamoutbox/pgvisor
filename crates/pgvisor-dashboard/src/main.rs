use std::sync::Arc;

use anyhow::Result;
use pgvisor_dashboard::{
    create_client_router, resolve_listen_addr, ClientDashboardState, SidecarClient,
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let bind_addr = resolve_listen_addr()?;
    let cluster_id =
        std::env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-primary".into());
    let sidecar_client = Arc::new(SidecarClient::from_env()?);
    let state = Arc::new(ClientDashboardState::new(sidecar_client.clone(), &cluster_id));
    let app = create_client_router(state);

    info!(
        addr = %bind_addr,
        sidecars = ?sidecar_client.configured_urls(),
        "PgVisor standalone dashboard UI server listening"
    );
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
