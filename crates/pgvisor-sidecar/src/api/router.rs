use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use super::api_state::ApiState;
use super::audit::get_audit_logs;
use super::auth::{api_auth_middleware, get_me, post_login};
use super::cluster::{get_node_config, get_node_logs, get_nodes, get_status};
use super::members::get_cluster_members;
use super::metrics::{get_metrics_history, get_metrics_snapshot};
use super::sql::{execute_sql, get_table_data, get_table_schema, list_databases, list_tables};
use super::users::{get_user, get_user_privileges, list_users};

/// Constructs the client-facing `/api/*` Axum router (served on `PGVISOR_API_PORT`).
pub fn build_api_router(state: ApiState) -> Router {
    let public_routes = Router::new().route("/api/auth/login", post(post_login));

    let protected_routes = Router::new()
        .route("/api/auth/me", get(get_me))
        .route("/api/cluster/members", get(get_cluster_members))
        .route("/api/status", get(get_status))
        .route("/api/nodes", get(get_nodes))
        .route("/api/nodes/:node_id/logs", get(get_node_logs))
        .route(
            "/api/nodes/:node_id/config/:config_type",
            get(get_node_config),
        )
        .route("/api/audit-logs", get(get_audit_logs))
        .route("/api/metrics/snapshot", get(get_metrics_snapshot))
        .route("/api/metrics/history", get(get_metrics_history))
        .route("/api/databases", get(list_databases))
        .route("/api/tables", get(list_tables))
        .route("/api/tables/:table/schema", get(get_table_schema))
        .route("/api/tables/:table/data", get(get_table_data))
        .route("/api/sql", post(execute_sql))
        .route("/api/users", get(list_users))
        .route("/api/users/:role", get(get_user))
        .route("/api/users/:role/privileges", get(get_user_privileges))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api_auth_middleware,
        ));

    public_routes.merge(protected_routes).with_state(state)
}
