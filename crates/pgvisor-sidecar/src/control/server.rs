use std::net::SocketAddr;

use axum::routing::{get, post};
use axum::Router;
use tracing::{info, warn};

use super::auth::cluster_auth_middleware;
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            cluster_auth_middleware,
        ))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use super::*;
    use crate::config::PostgresConfig;
    use crate::supervisor::PostgresSupervisor;

    fn create_test_state(secret: Option<String>) -> SidecarState {
        let temp_dir = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
        SidecarState::new(
            supervisor,
            Arc::new(RwLock::new(PostgresConfig::default())),
            None,
            1,
            Arc::new(RwLock::new("leader".to_string())),
            "18.6".to_string(),
            Arc::new(Vec::new()),
            secret,
        )
    }

    #[tokio::test]
    async fn test_control_router_requires_auth_on_all_endpoints_when_configured() {
        let secret = "cluster-secret-test";
        let state = create_test_state(Some(secret.to_string()));
        let app = build_control_router(state);

        // GET /control/status without auth -> 401
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // POST /control/fence without auth -> 401
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/fence")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // GET /control/status with valid auth -> 200
        let token_header = pgvisor_core::auth::make_auth_header_value(secret);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .header("authorization", token_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_control_router_open_when_no_secret() {
        let state = create_test_state(None);
        let app = build_control_router(state);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
