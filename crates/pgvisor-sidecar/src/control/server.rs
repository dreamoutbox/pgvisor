use std::net::SocketAddr;

use axum::routing::{get, post};
use axum::Router;
use tracing::{info, warn};

use super::auth::cluster_auth_middleware;
use super::handlers::{
    handle_acquire_backup_lock, handle_cancel_restore, handle_config, handle_demote, handle_events,
    handle_fence, handle_logs, handle_prepare_restore, handle_promote, handle_release_backup_lock,
    handle_repoint, handle_restart, handle_restore, handle_resync, handle_start, handle_status,
    handle_stop,
};
use super::state::SidecarState;

/// Constructs the Axum router for the sidecar control API.
pub fn build_control_router(state: SidecarState) -> Router {
    Router::new()
        .route("/control/status", get(handle_status))
        .route("/control/events", get(handle_events))
        .route("/control/logs", get(handle_logs))
        .route("/control/config/:config_type", get(handle_config))
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
        .route(
            "/control/backup-lock/acquire",
            post(handle_acquire_backup_lock),
        )
        .route(
            "/control/backup-lock/release",
            post(handle_release_backup_lock),
        )
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
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
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

    #[tokio::test]
    async fn test_backup_lock_rejects_standby() {
        let state = create_test_state(None);
        {
            let mut r = state.role.write().await;
            *r = "standby".to_string();
        }
        let app = build_control_router(state);

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_backup_lock_exclusive_and_release() {
        let state = create_test_state(None);
        let app = build_control_router(state);

        // First acquire should succeed (200 OK)
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let acquire_resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let token = acquire_resp["lock_token"].as_str().unwrap().to_string();
        assert!(!token.is_empty());
        assert!(acquire_resp.get("expires_at").is_some());

        // Second acquire while lock held should fail (409 Conflict)
        let res2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::CONFLICT);

        // Release with valid token
        let release_payload = serde_json::json!({ "lock_token": token });
        let res3 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/release")
                    .header("content-type", "application/json")
                    .body(Body::from(release_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res3.status(), StatusCode::OK);

        // After release, third acquire should succeed
        let res4 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res4.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_backup_lock_mismatch_token_does_not_release() {
        let state = create_test_state(None);
        let app = build_control_router(state);

        // Acquire lock
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Release with bogus token
        let release_payload = serde_json::json!({ "lock_token": "wrong-token-uuid" });
        let res2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/release")
                    .header("content-type", "application/json")
                    .body(Body::from(release_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::OK);

        // Lock should STILL be held -> next acquire returns 409
        let res3 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res3.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_backup_lock_auto_expires_after_ttl() {
        let state = create_test_state(None);
        // Artificially inject an already-expired lock
        {
            let mut lock_guard = state.backup_lock.write().await;
            *lock_guard = Some(crate::control::state::BackupLockInfo {
                token: "old-expired-token".to_string(),
                acquired_at: chrono::Utc::now() - chrono::Duration::minutes(30),
                expires_at: chrono::Utc::now() - chrono::Duration::minutes(15),
            });
        }
        let app = build_control_router(state);

        // Acquire should detect expired lock, auto-release, and succeed
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/control/backup-lock/acquire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_control_router_logs() {
        let state = create_test_state(None);
        state
            .supervisor
            .append_log(pgvisor_core::LogLevel::Info, "server started")
            .await;
        let app = build_control_router(state);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/control/logs?limit=50")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let logs_resp: pgvisor_core::node::NodeLogsResponse =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(logs_resp.node_id, 1);
        assert_eq!(logs_resp.total_buffered, 1);
        assert_eq!(logs_resp.entries[0].message, "server started");
    }

    #[tokio::test]
    async fn test_control_router_config() {
        let state = create_test_state(None);
        let app = build_control_router(state);

        // Valid slug for non-existent file
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/control/config/postgresql_conf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let cfg_resp: pgvisor_core::node::NodeConfigResponse =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(cfg_resp.filename, "postgresql.conf");
        assert!(!cfg_resp.exists);

        // Invalid slug -> 400 Bad Request
        let res2 = app
            .oneshot(
                Request::builder()
                    .uri("/control/config/unknown_file_type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
    }
}
