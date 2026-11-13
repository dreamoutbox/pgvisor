use std::net::SocketAddr;

use axum::Router;
use tracing::{info, warn};

use super::error::ApiError;

/// Validates that the internal control port and client API port do not collide.
///
/// # Arguments
/// * `control_port` - Port bound by the internal `/control/*` router.
/// * `api_port` - Port bound by the client-facing `/api/*` router.
pub fn resolve_sidecar_ports(control_port: u16, api_port: u16) -> Result<(u16, u16), ApiError> {
    if control_port == api_port {
        return Err(ApiError::PortCollision(api_port));
    }
    Ok((control_port, api_port))
}

/// Spawns the client-facing `/api/*` HTTP server task in the background.
pub fn spawn_api_server(addr: SocketAddr, app: Router) {
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                info!(%addr, "pgvisor-sidecar API server listening");
                let _ = axum::serve(listener, app).await;
            }
            Err(e) => {
                warn!(%addr, ?e, "Failed to bind sidecar API server");
            }
        }
    });
}
