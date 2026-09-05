pub mod handlers;
pub mod models;
pub mod security;
pub mod templates;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use std::sync::Arc;

use crate::handlers::{
    api_create_backup, api_delete_backup, api_download_backup, api_execute_sql, api_list_backups,
    api_list_tables, api_restore_backup, api_status, api_table_data, api_table_schema,
    get_backups_page, get_login_page, get_logout, get_nodes, get_overview, get_sql_console,
    get_tables_page, post_login, DashboardState,
};
use crate::security::SqlSecurityGuard;

/// Authentication middleware enforcing PGVISOR_ADMIN_TOKEN when configured.
pub async fn auth_middleware(
    State(state): State<Arc<DashboardState>>,
    req: Request,
    next: Next,
) -> Response {
    // If admin_token is not configured, auth is disabled -> pass through
    let Some(expected_token) = state.admin_token.as_deref() else {
        return next.run(req).await;
    };

    let path = req.uri().path();
    // Allow /login and /logout to proceed without prior auth
    if path == "/login" || path == "/logout" {
        return next.run(req).await;
    }

    // Check token from headers (Authorization Bearer, X-Admin-Token, or Cookie)
    if let Some(token) = SqlSecurityGuard::extract_token_from_headers(req.headers()) {
        if token.trim() == expected_token.trim() {
            return next.run(req).await;
        }
    }

    // Reject unauthenticated requests
    if path.starts_with("/api/") {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized: invalid or missing admin token",
                "status": "unauthorized"
            })),
        )
            .into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

/// Creates the Axum router for the PgVisor dashboard.
pub fn create_router(state: Arc<DashboardState>) -> Router {
    Router::new()
        .route("/login", get(get_login_page).post(post_login))
        .route("/logout", get(get_logout).post(get_logout))
        .route("/", get(get_overview))
        .route("/nodes", get(get_nodes))
        .route("/tables", get(get_tables_page))
        .route("/sql", get(get_sql_console))
        .route("/backups", get(get_backups_page))
        .route("/api/status", get(api_status))
        .route("/api/sql", post(api_execute_sql))
        .route("/api/tables", get(api_list_tables))
        .route("/api/tables/:table/schema", get(api_table_schema))
        .route("/api/tables/:table/data", get(api_table_data))
        .route(
            "/api/backups",
            get(api_list_backups).post(api_create_backup),
        )
        .route(
            "/api/backups/:snapshot_id/download",
            get(api_download_backup),
        )
        .route(
            "/api/backups/:snapshot_id/restore",
            post(api_restore_backup),
        )
        .route("/api/backups/:snapshot_id", delete(api_delete_backup))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    #[tokio::test]
    async fn test_dashboard_routes_and_sql_guard() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state);

        // 1. Test GET /
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 2. Test GET /nodes
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 3. Test GET /sql
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/sql").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 4. Test POST /api/sql with safe SELECT
        let req = Request::builder()
            .method("POST")
            .uri("/api/sql")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query": "SELECT 1;"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 5. Test POST /api/sql with DROP TABLE when mutations are allowed
        let req = Request::builder()
            .method("POST")
            .uri("/api/sql")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query": "DROP TABLE users;"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 5b. Test POST /api/sql with read-only guard rejecting DROP TABLE
        let mut ro_state_inner = DashboardState::new("test-cluster", None);
        ro_state_inner.security_guard = Arc::new(crate::security::SqlSecurityGuard::new_read_only(
            std::time::Duration::from_secs(5),
            100,
        ));
        let ro_app = create_router(Arc::new(ro_state_inner));
        let req_ro = Request::builder()
            .method("POST")
            .uri("/api/sql")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query": "DROP TABLE users;"}"#))
            .unwrap();
        let response_ro = ro_app.oneshot(req_ro).await.unwrap();
        assert_eq!(response_ro.status(), StatusCode::FORBIDDEN);

        // 6. Test GET /tables
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/tables")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 7. Test GET /api/tables
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tables")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 8. Test GET /api/tables/pgvisor_demo/schema
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tables/pgvisor_demo/schema")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 9. Test GET /api/tables/pgvisor_demo/data
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tables/pgvisor_demo/data")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 10. Test GET /backups page
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/backups")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 11. Test GET /api/backups
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/backups")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 12. Test POST /api/backups
        let req = Request::builder()
            .method("POST")
            .uri("/api/backups")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"backup_type": "full", "label": "test-snap"}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 13. Test POST /api/backups/snap-20260904-200000/restore
        let req = Request::builder()
            .method("POST")
            .uri("/api/backups/snap-20260904-200000/restore")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 14. Test DELETE /api/backups/snap-20260904-200000
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/backups/snap-20260904-200000")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dashboard_auth_enforcement() {
        let token = "pgvisor-secret-key-42";
        let state = Arc::new(DashboardState::new("auth-cluster", Some(token.to_string())));
        let app = create_router(state);

        // 1. Unauthenticated request to / should redirect to /login
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap(),
            "/login"
        );

        // 2. Unauthenticated request to /api/status should return 401 Unauthorized
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // 3. Unauthenticated POST to /api/sql should return 401 Unauthorized
        let req = Request::builder()
            .method("POST")
            .uri("/api/sql")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query": "SELECT 1;"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // 4. API request with wrong Bearer token should return 401 Unauthorized
        let req = Request::builder()
            .uri("/api/status")
            .header("Authorization", "Bearer invalid-token")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // 5. API request with valid Bearer token should succeed (200 OK)
        let req = Request::builder()
            .uri("/api/status")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 6. API request with valid X-Admin-Token header should succeed (200 OK)
        let req = Request::builder()
            .uri("/api/status")
            .header("x-admin-token", token)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 7. GET /login should render login page (200 OK)
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 8. POST /login with invalid token should fail (401 Unauthorized)
        let req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from("token=wrong-pass"))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // 9. POST /login with valid form token should redirect to / and set cookie
        let req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("token={}", token)))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap(),
            "/"
        );
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains(&format!("pgvisor_token={}", token)));

        // 10. POST /login with JSON payload should return 200 OK and set cookie
        let req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("Content-Type", "application/json")
            .body(Body::from(format!(r#"{{"token": "{}"}}"#, token)))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains(&format!("pgvisor_token={}", token)));

        // 11. Request to / with valid pgvisor_token cookie should succeed (200 OK)
        let req = Request::builder()
            .uri("/")
            .header("Cookie", format!("pgvisor_token={}", token))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 12. Request to /logout should redirect to /login and clear cookie
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap(),
            "/login"
        );
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("Max-Age=0"));
    }
}
