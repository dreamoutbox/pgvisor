pub mod handlers;
pub mod models;
pub mod security;
pub mod templates;

use std::sync::Arc;
use axum::routing::{delete, get, post};
use axum::Router;

use crate::handlers::{
    api_create_backup, api_delete_backup, api_download_backup, api_execute_sql, api_list_backups,
    api_list_tables, api_restore_backup, api_status, api_table_data, api_table_schema,
    get_backups_page, get_nodes, get_overview, get_sql_console, get_tables_page, DashboardState,
};

/// Creates the Axum router for the PgVisor dashboard.
pub fn create_router(state: Arc<DashboardState>) -> Router {
    Router::new()
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
        .route("/api/backups", get(api_list_backups).post(api_create_backup))
        .route("/api/backups/:snapshot_id/download", get(api_download_backup))
        .route("/api/backups/:snapshot_id/restore", post(api_restore_backup))
        .route("/api/backups/:snapshot_id", delete(api_delete_backup))
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
            .oneshot(Request::builder().uri("/nodes").body(Body::empty()).unwrap())
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
            .oneshot(Request::builder().uri("/tables").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 7. Test GET /api/tables
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/api/tables").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 8. Test GET /api/tables/pgvisor_demo/schema
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/api/tables/pgvisor_demo/schema").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 9. Test GET /api/tables/pgvisor_demo/data
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/api/tables/pgvisor_demo/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 10. Test GET /backups page
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/backups").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 11. Test GET /api/backups
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/api/backups").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 12. Test POST /api/backups
        let req = Request::builder()
            .method("POST")
            .uri("/api/backups")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"backup_type": "full", "label": "test-snap"}"#))
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
}
