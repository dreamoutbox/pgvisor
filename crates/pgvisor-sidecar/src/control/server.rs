use std::net::SocketAddr;

use axum::routing::{get, post};
use axum::Router;
use tracing::{info, warn};

use super::handlers::{
    handle_cancel_restore, handle_demote, handle_events, handle_fence, handle_prepare_restore,
    handle_promote, handle_repoint, handle_restart, handle_restore, handle_resync, handle_start,
    handle_status, handle_stop,
};
use super::state::SidecarState;

/// Constructs the Axum router for the sidecar control API.
pub fn build_control_router(state: SidecarState) -> Router {
    Router::new()
        .route("/control/status", get(handle_status))
        .route("/control/events", get(handle_events))
        .route("/control/start", post(handle_start))
        .route("/control/stop", post(handle_stop))
        .route("/control/restart", post(handle_restart))
        .route("/control/restore", post(handle_restore))
        .route("/control/prepare-restore", post(handle_prepare_restore))
        .route("/control/cancel-restore", post(handle_cancel_restore))
        .route("/control/resync", post(handle_resync))
        .route("/control/promote", post(handle_promote))
        .route("/control/fence", post(handle_fence))
        .route("/control/demote", post(handle_demote))
        .route("/control/repoint", post(handle_repoint))
        .with_state(state)
}

/// Spawns the HTTP control server task in the background.
pub fn spawn_control_server(addr: SocketAddr, app: Router) {
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                info!(%addr, "pgvisor-sidecar control server listening");
                let _ = axum::serve(listener, app).await;
            }
            Err(e) => {
                warn!(%addr, ?e, "Failed to bind sidecar control server");
            }
        }
    });
}
