use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get};
use axum::response::Redirect;
use axum::Router;

use super::auth_handlers::{client_get_login_page, client_logout, client_post_login};
use super::client_dashboard_state::ClientDashboardState;
use super::page_handlers::{
    client_get_audit_logs, client_get_backups, client_get_node_inspect, client_get_nodes,
    client_get_overview, client_get_sql_console, client_get_tables, client_get_users,
};
use super::proxy_handlers::proxy_api_request;

pub const DEFAULT_DASHBOARD_LISTEN: &str = "0.0.0.0:8080";
pub const DASHBOARD_LISTEN_ENV: &str = "PGVISOR_DASHBOARD_LISTEN";

/// Returns the maximum allowed backup upload size in bytes (defaults to 1 GiB).
fn max_upload_bytes() -> usize {
    std::env::var("PGVISOR_BACKUP_MAX_UPLOAD_BYTES")
        .or_else(|_| std::env::var("BACKUP_MAX_UPLOAD_BYTES"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1024 * 1024 * 1024)
}

/// Resolves the HTTP listen socket address from `PGVISOR_DASHBOARD_LISTEN` (default `0.0.0.0:8080`).
pub fn resolve_listen_addr() -> Result<SocketAddr, std::net::AddrParseError> {
    let raw = std::env::var(DASHBOARD_LISTEN_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DASHBOARD_LISTEN.to_string());
    raw.trim().parse::<SocketAddr>()
}

/// Constructs the standalone UI-only Axum router backed by `pgvisor-sidecar` `/api/*`.
pub fn create_client_router(state: Arc<ClientDashboardState>) -> Router {
    let body_limit = max_upload_bytes().saturating_add(10 * 1024 * 1024);

    Router::new()
        .route(
            "/login",
            get(client_get_login_page).post(client_post_login),
        )
        .route("/logout", get(client_logout).post(client_logout))
        .route("/", get(client_get_overview))
        .route("/nodes", get(client_get_nodes))
        .route("/nodes/:node_id/inspect", get(client_get_node_inspect))
        .route("/nodes/:node_id", get(client_get_node_inspect))
        .route("/metrics", get(|| async { Redirect::to("/") }))
        .route("/tables", get(client_get_tables))
        .route("/sql", get(client_get_sql_console))
        .route("/backups", get(client_get_backups))
        .route("/users", get(client_get_users))
        .route("/audit-logs", get(client_get_audit_logs))
        .route(
            "/api/*path",
            any(proxy_api_request).layer(DefaultBodyLimit::max(body_limit)),
        )
        .with_state(state)
}
