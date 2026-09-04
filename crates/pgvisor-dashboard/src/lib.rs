pub mod handlers;
pub mod models;
pub mod security;
pub mod templates;

use std::sync::Arc;
use axum::routing::{get, post};
use axum::Router;

use crate::handlers::{
    api_execute_sql, api_status, get_nodes, get_overview, get_sql_console, DashboardState,
};

/// Creates the Axum router for the PgVisor dashboard.
pub fn create_router(state: Arc<DashboardState>) -> Router {
    Router::new()
        .route("/", get(get_overview))
        .route("/nodes", get(get_nodes))
        .route("/sql", get(get_sql_console))
        .route("/api/status", get(api_status))
        .route("/api/sql", post(api_execute_sql))
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

        // 5. Test POST /api/sql with forbidden DROP TABLE
        let req = Request::builder()
            .method("POST")
            .uri("/api/sql")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query": "DROP TABLE users;"}"#))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
