use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};
use axum::Router;

use super::api_state::ApiState;
use super::audit::get_audit_logs;
use super::auth::{api_auth_middleware, get_me, post_login};
use super::backup::{
    create_backup, delete_backup, download_backup, find_best_backup, list_backups,
    max_upload_bytes, pitr_window, quick_restore, restore_backup, upload_backup,
};
use super::cluster::{get_node_config, get_node_logs, get_nodes, get_status};
use super::cluster_actions::{
    post_node_action, post_repoint_node, post_restart_node, post_start_node, post_stop_node,
    post_switchover,
};
use super::members::get_cluster_members;
use super::metrics::{get_metrics_history, get_metrics_snapshot};
use super::sql::{
    delete_table_row, execute_sql, get_table_data, get_table_schema, list_databases, list_tables,
    update_table_row,
};
use super::users::{
    alter_user, create_user, drop_user, get_user, get_user_privileges, grant_membership,
    list_users, revoke_membership, set_user_privilege,
};

/// Constructs the client-facing `/api/*` Axum router (served on `PGVISOR_API_PORT`).
pub fn build_api_router(state: ApiState) -> Router {
    let public_routes = Router::new().route("/api/auth/login", post(post_login));

    let protected_routes = Router::new()
        .route("/api/auth/me", get(get_me))
        .route("/api/cluster/members", get(get_cluster_members))
        .route("/api/cluster/switchover", post(post_switchover))
        .route("/api/status", get(get_status))
        .route("/api/nodes", get(get_nodes))
        .route("/api/nodes/:node_id/start", post(post_start_node))
        .route("/api/nodes/:node_id/stop", post(post_stop_node))
        .route("/api/nodes/:node_id/restart", post(post_restart_node))
        .route("/api/nodes/:node_id/action", post(post_node_action))
        .route("/api/nodes/:node_id/repoint", post(post_repoint_node))
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
        .route(
            "/api/tables/:table/rows",
            put(update_table_row).delete(delete_table_row),
        )
        .route("/api/sql", post(execute_sql))
        .route("/api/users", get(list_users).post(create_user))
        .route(
            "/api/users/:role",
            get(get_user).put(alter_user).delete(drop_user),
        )
        .route("/api/users/:role/memberships", post(grant_membership))
        .route(
            "/api/users/:role/memberships/:group",
            axum::routing::delete(revoke_membership),
        )
        .route(
            "/api/users/:role/privileges",
            get(get_user_privileges).post(set_user_privilege),
        )
        .route("/api/backups", get(list_backups).post(create_backup))
        .route("/api/backups/best", get(find_best_backup))
        .route("/api/backups/quick-restore", post(quick_restore))
        .route(
            "/api/backups/upload",
            post(upload_backup).layer(DefaultBodyLimit::max(
                max_upload_bytes().saturating_add(10 * 1024 * 1024),
            )),
        )
        .route(
            "/api/backups/:snapshot_id",
            axum::routing::delete(delete_backup),
        )
        .route("/api/backups/:snapshot_id/download", get(download_backup))
        .route("/api/backups/:snapshot_id/restore", post(restore_backup))
        .route("/api/backups/:snapshot_id/pitr-window", get(pitr_window))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api_auth_middleware,
        ));

    public_routes.merge(protected_routes).with_state(state)
}
