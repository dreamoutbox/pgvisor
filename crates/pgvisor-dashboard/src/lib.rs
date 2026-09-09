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
    api_alter_user, api_create_backup, api_create_user, api_delete_backup, api_download_backup,
    api_drop_user, api_execute_sql, api_get_privileges, api_get_user, api_grant_membership,
    api_list_audit_logs, api_list_backups, api_list_tables, api_list_users, api_nodes,
    api_restore_backup, api_revoke_membership, api_set_privilege, api_status, api_switchover,
    api_table_data, api_table_schema, get_audit_logs_page, get_backups_page, get_login_page,
    get_logout, get_nodes, get_overview, get_sql_console, get_tables_page, get_users_page,
    post_login, DashboardState,
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
        .route("/users", get(get_users_page))
        .route("/audit-logs", get(get_audit_logs_page))
        .route("/api/status", get(api_status))
        .route("/api/nodes", get(api_nodes))
        .route("/api/audit-logs", get(api_list_audit_logs))
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
}
