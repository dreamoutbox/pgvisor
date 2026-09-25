pub mod handlers;
pub mod metrics;
pub mod models;
pub mod security;
pub mod templates;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use std::sync::Arc;

use crate::handlers::{
    api_alter_user, api_create_backup, api_create_user, api_delete_backup, api_download_backup,
    api_drop_user, api_execute_sql, api_find_best_backup, api_get_privileges, api_get_user,
    api_grant_membership, api_list_audit_logs, api_list_backups, api_list_tables, api_list_users,
    api_metrics_history, api_metrics_snapshot, api_node_action, api_node_config, api_node_logs,
    api_nodes, api_quick_restore, api_restart_node, api_restore_backup, api_revoke_membership,
    api_set_privilege, api_start_node, api_status, api_stop_node, api_switchover, api_table_data,
    api_table_schema, api_delete_table_row, api_update_table_row, get_audit_logs_page, get_backups_page, get_login_page, get_logout,
    get_node_inspect_page, get_nodes, get_overview, get_sql_console, get_tables_page, get_users_page, post_login,
    DashboardState,
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
        .route("/nodes/:node_id/inspect", get(get_node_inspect_page))
        .route("/nodes/:node_id", get(get_node_inspect_page))
        .route("/metrics", get(|| async { Redirect::to("/") }))
        .route("/tables", get(get_tables_page))
        .route("/sql", get(get_sql_console))
        .route("/backups", get(get_backups_page))
        .route("/users", get(get_users_page))
        .route("/audit-logs", get(get_audit_logs_page))
        .route("/api/status", get(api_status))
        .route("/api/nodes", get(api_nodes))
        .route("/api/nodes/:node_id/start", post(api_start_node))
        .route("/api/nodes/:node_id/stop", post(api_stop_node))
        .route("/api/nodes/:node_id/restart", post(api_restart_node))
        .route("/api/nodes/:node_id/action", post(api_node_action))
        .route("/api/nodes/:node_id/logs", get(api_node_logs))
        .route("/api/nodes/:node_id/config/:config_type", get(api_node_config))
        .route("/api/metrics/snapshot", get(api_metrics_snapshot))
        .route("/api/metrics/history", get(api_metrics_history))
        .route("/api/audit-logs", get(api_list_audit_logs))
        .route("/api/sql", post(api_execute_sql))
        .route("/api/tables", get(api_list_tables))
        .route("/api/tables/:table/schema", get(api_table_schema))
        .route("/api/tables/:table/data", get(api_table_data))
        .route(
            "/api/tables/:table/rows",
            put(api_update_table_row).delete(api_delete_table_row),
        )
        .route(
            "/api/backups",
            get(api_list_backups).post(api_create_backup),
        )
        .route("/api/backups/quick-restore", post(api_quick_restore))
        .route("/api/backups/best", get(api_find_best_backup))
        .route(
            "/api/backups/:snapshot_id/download",
            get(api_download_backup),
        )
        .route(
            "/api/backups/:snapshot_id/restore",
            post(api_restore_backup),
        )
        .route("/api/backups/:snapshot_id", delete(api_delete_backup))
        .route("/api/cluster/switchover", post(api_switchover))
        .route("/api/users", get(api_list_users).post(api_create_user))
        .route(
            "/api/users/:role",
            get(api_get_user).put(api_alter_user).delete(api_drop_user),
        )
        .route("/api/users/:role/memberships", post(api_grant_membership))
        .route(
            "/api/users/:role/memberships/:group",
            delete(api_revoke_membership),
        )
        .route(
            "/api/users/:role/privileges",
            get(api_get_privileges).post(api_set_privilege),
        )
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

        // 14a. Test GET /api/backups/best
        let now_str = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let uri = format!(
            "/api/backups/best?target_time={}",
            now_str.replace(' ', "%20")
        );
        let req = Request::builder()
            .method("GET")
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 14b. Test POST /api/backups/quick-restore
        let req = Request::builder()
            .method("POST")
            .uri("/api/backups/quick-restore")
            .header("Content-Type", "application/json")
            .body(Body::from(format!(
                r#"{{"recovery_target_time": "{}"}}"#,
                now_str
            )))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 14c. Test POST /api/backups/quick-restore with invalid time
        let req = Request::builder()
            .method("POST")
            .uri("/api/backups/quick-restore")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"recovery_target_time": "invalid-time"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // 15. Test GET /users page
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 16. Test GET /api/users
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 17. Test GET /api/users/app_user
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/users/app_user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 18. Test POST /api/users (create new role)
        let req = Request::builder()
            .method("POST")
            .uri("/api/users")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"name": "test_dev", "login": true, "createdb": true, "connection_limit": 20}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 19. Test PUT /api/users/test_dev (alter role)
        let req = Request::builder()
            .method("PUT")
            .uri("/api/users/test_dev")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"connection_limit": 10}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 20. Test POST /api/users/test_dev/memberships (grant membership)
        let req = Request::builder()
            .method("POST")
            .uri("/api/users/test_dev/memberships")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"member_role": "test_dev", "group_role": "read_only_group"}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 21. Test GET /api/users/test_dev/privileges
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/users/test_dev/privileges")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 22. Test POST /api/users/test_dev/privileges (set privilege)
        let req = Request::builder()
            .method("POST")
            .uri("/api/users/test_dev/privileges")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"table_name": "pgvisor_demo", "privilege": "select", "grant": true}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 23. Test DELETE /api/users/test_dev/memberships/read_only_group
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/users/test_dev/memberships/read_only_group")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 24. Test DELETE /api/users/test_dev
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/users/test_dev")
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

    #[tokio::test]
    async fn test_dashboard_switchover_route() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state);

        // 1. Switchover to healthy standby (Node #2)
        let req = Request::builder()
            .method("POST")
            .uri("/api/cluster/switchover")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"target_node_id": 2}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 2. Switchover to current leader (Node #2 is now leader) -> 400 Bad Request
        let req = Request::builder()
            .method("POST")
            .uri("/api/cluster/switchover")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"target_node_id": 2}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // 3. Switchover to non-existent node -> 404 Not Found
        let req = Request::builder()
            .method("POST")
            .uri("/api/cluster/switchover")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"target_node_id": 99}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dashboard_node_lifecycle_routes() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state);

        // 1. Stop healthy Node #2 -> 200 OK
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/stop")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 2. Stop Node #2 again -> 400 Bad Request (already stopped)
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/stop")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // 3. Start Node #2 -> 200 OK
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/start")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 4. Start Node #2 again -> 400 Bad Request (already running)
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/start")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // 5. Restart Node #2 -> 200 OK
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/restart")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 6. Action route with payload on Node #2 -> 200 OK
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/2/action")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"action": "stop"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 7. Action on non-existent node #99 -> 404 Not Found
        let req = Request::builder()
            .method("POST")
            .uri("/api/nodes/99/start")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dashboard_metrics_routes() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state);

        // 1. GET / returns HTML page with cluster overview, node topology, and charts
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Cluster Metrics &amp; Telemetry"));
        assert!(html.contains("uptimeChart"));
        assert!(html.contains("nodeQueriesChart"));

        // 2. GET /metrics redirects to /
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
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

        // 2. GET /api/metrics/snapshot returns JSON snapshot
        let req = Request::builder()
            .uri("/api/metrics/snapshot")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let snap: crate::models::ClusterMetricsSnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(snap.nodes.len(), 3);

        // 3. GET /api/metrics/history returns JSON history
        let req = Request::builder()
            .uri("/api/metrics/history")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let hist: Vec<crate::models::ClusterMetricsSnapshot> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(hist.len(), 60);

        // 4. Auth protection when token configured
        let auth_state = Arc::new(DashboardState::new(
            "test-cluster",
            Some("secret123".into()),
        ));
        let auth_app = create_router(auth_state);

        // Unauthenticated /metrics redirects to /login
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = auth_app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // Unauthenticated /api/metrics/snapshot returns 401 Unauthorized
        let req = Request::builder()
            .uri("/api/metrics/snapshot")
            .body(Body::empty())
            .unwrap();
        let response = auth_app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Authenticated /api/metrics/snapshot with Bearer token succeeds
        let req = Request::builder()
            .uri("/api/metrics/snapshot")
            .header("Authorization", "Bearer secret123")
            .body(Body::empty())
            .unwrap();
        let response = auth_app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dashboard_node_inspection_routes() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state);

        // 1. GET /api/nodes/1/logs returns logs
        let req = Request::builder()
            .uri("/api/nodes/1/logs?limit=2")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let logs: crate::models::NodeLogsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(logs.node_id, 1);
        assert_eq!(logs.entries.len(), 2);

        // 2. GET /api/nodes/1/config/postgresql_conf returns config
        let req = Request::builder()
            .uri("/api/nodes/1/config/postgresql_conf")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let cfg: crate::models::NodeConfigResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(cfg.filename, "postgresql.conf");
        assert_eq!(cfg.path, "/var/lib/postgresql/data/postgresql.conf");
        assert!(cfg.content.contains("listen_addresses"));

        // 3. GET /api/nodes/1/config/invalid_slug returns 400
        let req = Request::builder()
            .uri("/api/nodes/1/config/invalid_slug")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 4. GET /nodes HTML contains Inspect link to /nodes/1/inspect (no modal)
        let req = Request::builder()
            .uri("/nodes")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("/nodes/1/inspect"));
        assert!(!html.contains("inspectModal"));

        // 5. GET /nodes/1/inspect dedicated page returns 200 OK with diagnostics UI
        let req = Request::builder()
            .uri("/nodes/1/inspect")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Node #1 Diagnostics"));
        assert!(html.contains("inspectLogsContainer"));
        assert!(html.contains("inspectConfigContainer"));

        // 6. GET /nodes/999/inspect for non-existent node returns 404 Not Found
        let req = Request::builder()
            .uri("/nodes/999/inspect")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dashboard_table_row_mutation_routes() {
        let state = Arc::new(DashboardState::new("test-cluster", None));
        let app = create_router(state.clone());

        // 1. DELETE /api/tables/pgvisor_demo/rows with valid primary key succeeds
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {"id": 1}}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let mutation: crate::models::RowMutationResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(mutation.status, "ok");
        assert_eq!(mutation.affected_rows, 1);

        // 2. PUT /api/tables/pgvisor_demo/rows with valid primary key and values succeeds
        let req = Request::builder()
            .method("PUT")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"primary_keys": {"id": 1}, "values": {"name": "updated_alpha", "counter": 42}}"#,
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let mutation: crate::models::RowMutationResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(mutation.status, "ok");
        assert_eq!(mutation.affected_rows, 1);

        // 3. Validation: Invalid table name returns 400
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/bad;drop/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {"id": 1}}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 4. Validation: Empty primary keys payload returns 400
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {}}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 5. Validation: Null primary key value returns 400
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {"id": null}}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 6. Validation: Attempt to update primary key in values returns 400
        let req = Request::builder()
            .method("PUT")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"primary_keys": {"id": 1}, "values": {"id": 2}}"#,
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 7. Validation: Empty values in update returns 400
        let req = Request::builder()
            .method("PUT")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {"id": 1}, "values": {}}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 8. Validation: Malicious column identifier in update returns 400
        let req = Request::builder()
            .method("PUT")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"primary_keys": {"id": 1}, "values": {"name'; DROP TABLE x;--": "exploit"}}"#,
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 9. Verify AuditLog entries were recorded
        let (logs, _) = state.audit_log.list(10, 0, None, None).await;
        assert!(logs.iter().any(|e| e.detail.contains("Deleted row from table 'pgvisor_demo'")));
        assert!(logs.iter().any(|e| e.detail.contains("Updated row in table 'pgvisor_demo'")));

        // 10. Auth protection when token configured
        let auth_state = Arc::new(DashboardState::new(
            "test-cluster",
            Some("secret123".into()),
        ));
        let auth_app = create_router(auth_state);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"primary_keys": {"id": 1}}"#))
            .unwrap();
        let resp = auth_app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/tables/pgvisor_demo/rows")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer secret123")
            .body(Body::from(r#"{"primary_keys": {"id": 1}}"#))
            .unwrap();
        let resp = auth_app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 11. GET /tables?table=pgvisor_demo&tab=data renders table actions and modals
        let req = Request::builder()
            .uri("/tables?table=pgvisor_demo&tab=data")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("deleteRowModal"));
        assert!(html.contains("editRowModal"));
        assert!(html.contains("row-edit-btn"));
        assert!(html.contains("row-delete-btn"));
        assert!(html.contains("table-metadata"));
    }
}
