use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pgvisor_core::auth::validate_bearer_token;

use super::state::SidecarState;

/// Authentication middleware enforcing PGVISOR_CLUSTER_SECRET when configured.
pub async fn cluster_auth_middleware(
    State(state): State<SidecarState>,
    req: Request,
    next: Next,
) -> Response {
    // If cluster_secret is not configured, auth is disabled -> pass through
    let Some(secret) = state.cluster_secret.as_deref() else {
        return next.run(req).await;
    };

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    if let Some(header_val) = auth_header {
        if validate_bearer_token(secret, header_val) {
            return next.run(req).await;
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Unauthorized: invalid or missing cluster authorization token",
            "status": "unauthorized"
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
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
    async fn test_auth_middleware_disabled_when_secret_none() {
        let state = create_test_state(None);
        let app = Router::new()
            .route("/control/status", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                cluster_auth_middleware,
            ))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_middleware_unauthorized_when_missing_header() {
        let state = create_test_state(Some("secret123".to_string()));
        let app = Router::new()
            .route("/control/status", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                cluster_auth_middleware,
            ))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_middleware_authorized_with_valid_token() {
        let secret = "secret123";
        let state = create_test_state(Some(secret.to_string()));
        let token_header = pgvisor_core::auth::make_auth_header_value(secret);

        let app = Router::new()
            .route("/control/status", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                cluster_auth_middleware,
            ))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .header("authorization", token_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_middleware_unauthorized_with_wrong_token() {
        let state = create_test_state(Some("secret123".to_string()));
        let token_header = pgvisor_core::auth::make_auth_header_value("wrong_secret");

        let app = Router::new()
            .route("/control/status", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                cluster_auth_middleware,
            ))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/control/status")
                    .header("authorization", token_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
