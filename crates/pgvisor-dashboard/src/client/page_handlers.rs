use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use pgvisor_core::api::{
    format_bytes, AuditEventView, AuditListResponse, AuditOverviewStats, BackupItemView,
    BackupOverviewSummary, ClusterOverview, ColumnInfo, NodeSummary, PgRole, TableDataResponse,
    TablePrivilege, TableSummary,
};
use pgvisor_core::backup::BasebackupMeta;
use serde::Deserialize;
use tracing::error;

use super::auth_handlers::{extract_client_token, redirect_to_login_clearing_cookie};
use super::client_dashboard_state::ClientDashboardState;
use super::error::SidecarClientError;
use crate::templates::{
    AuditTemplate, BackupsTemplate, NodeInspectTemplate, NodesTemplate, OverviewTemplate, PageItem,
    TablesTemplate, UsersTemplate,
};

#[derive(Debug, Default, Deserialize)]
pub struct ClientTablesQuery {
    pub db: Option<String>,
    pub table: Option<String>,
    pub tab: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub sql: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ClientUsersQuery {
    pub role: Option<String>,
    pub tab: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ClientAuditQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
}

fn map_client_err_to_response(err: SidecarClientError) -> Response {
    match err {
        SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_) => {
            redirect_to_login_clearing_cookie()
        }
        SidecarClientError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        SidecarClientError::Unavailable(msg) => {
            (StatusCode::BAD_GATEWAY, format!("Sidecar unavailable: {msg}")).into_response()
        }
        SidecarClientError::Upstream { status, message } => (status, message).into_response(),
        SidecarClientError::Decode(msg) => (StatusCode::BAD_GATEWAY, msg).into_response(),
        SidecarClientError::EmptyUrls => {
            (StatusCode::BAD_GATEWAY, "No sidecar API URLs configured").into_response()
        }
    }
}

fn render_template_or_500<T: Template>(template: &T, name: &str) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            error!(?err, template = name, "Failed to render dashboard template");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /` -> Renders cluster overview from sidecar `/api/status`.
pub async fn client_get_overview(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let mut overview: ClusterOverview = match state.sidecar_client.get_json("/api/status", &token).await {
        Ok(v) => v,
        Err(err) => return map_client_err_to_response(err),
    };

    if let Ok(backups) = state
        .sidecar_client
        .get_json::<Vec<BasebackupMeta>>("/api/backups", &token)
        .await
    {
        overview.total_backups = backups.len();
        overview.last_backup_at = backups.first().map(|b| b.created_at);
    }

    let template = OverviewTemplate {
        overview: &overview,
        auth_enabled: true,
    };
    render_template_or_500(&template, "overview.html")
}

/// `GET /nodes` -> Renders cluster node list from sidecar `/api/nodes`.
pub async fn client_get_nodes(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let nodes: Vec<NodeSummary> = match state.sidecar_client.get_json("/api/nodes", &token).await {
        Ok(v) => v,
        Err(err) => return map_client_err_to_response(err),
    };

    let template = NodesTemplate {
        nodes: &nodes,
        auth_enabled: true,
    };
    render_template_or_500(&template, "nodes.html")
}

/// `GET /nodes/:node_id/inspect` or `/nodes/:node_id` -> Renders node inspector page.
pub async fn client_get_node_inspect(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
    Path(node_id): Path<u64>,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let nodes: Vec<NodeSummary> = match state.sidecar_client.get_json("/api/nodes", &token).await {
        Ok(v) => v,
        Err(err) => return map_client_err_to_response(err),
    };
    let Some(node) = nodes.iter().find(|n| n.node_id == node_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let template = NodeInspectTemplate {
        node,
        auth_enabled: true,
    };
    render_template_or_500(&template, "node_inspect.html")
}

/// `GET /tables` -> Renders tables browser & SQL explorer from sidecar SQL/catalog endpoints.
pub async fn client_get_tables(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
    Query(params): Query<ClientTablesQuery>,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let databases: Vec<String> = match state.sidecar_client.get_json("/api/databases", &token).await {
        Ok(mut list) => {
            if Vec::is_empty(&list) {
                list.push("postgres".to_string());
            }
            list
        }
        Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
            return map_client_err_to_response(err);
        }
        Err(_) => vec!["postgres".to_string()],
    };

    let active_db = params
        .db
        .as_deref()
        .filter(|d| databases.iter().any(|candidate| candidate == d))
        .or_else(|| databases.first().map(String::as_str))
        .unwrap_or("postgres");

    let tables_path = format!("/api/tables?db={active_db}");
    let tables: Vec<TableSummary> = match state.sidecar_client.get_json(&tables_path, &token).await {
        Ok(v) => v,
        Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
            return map_client_err_to_response(err);
        }
        Err(_) => Vec::new(),
    };

    let active_tab = params.tab.as_deref().unwrap_or("data");
    let active_table = params.table.as_deref().or_else(|| {
        if active_tab == "sql" {
            None
        } else {
            tables.first().map(|t| t.name.as_str())
        }
    });

    let limit = params.limit.unwrap_or(50).clamp(1, 100);
    let page = params.page.unwrap_or(1).max(1);
    let mut columns: Vec<ColumnInfo> = Vec::new();
    let mut data_columns: Vec<String> = Vec::new();
    let mut data_rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut primary_keys: Vec<String> = Vec::new();
    let mut total_rows = 0u64;

    if let Some(tbl) = active_table {
        if active_tab == "schema" {
            let schema_path = format!("/api/tables/{tbl}/schema?db={active_db}");
            match state
                .sidecar_client
                .get_json::<Vec<ColumnInfo>>(&schema_path, &token)
                .await
            {
                Ok(cols) => {
                    primary_keys = cols
                        .iter()
                        .filter(|c| c.is_primary_key)
                        .map(|c| c.name.clone())
                        .collect();
                    columns = cols;
                }
                Err(
                    err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_)),
                ) => {
                    return map_client_err_to_response(err);
                }
                Err(_) => {}
            }
        } else if active_tab == "data" {
            let data_path = format!("/api/tables/{tbl}/data?db={active_db}&page={page}&limit={limit}");
            match state
                .sidecar_client
                .get_json::<TableDataResponse>(&data_path, &token)
                .await
            {
                Ok(data) => {
                    data_columns = data.columns;
                    data_rows = data.rows;
                    primary_keys = data.primary_keys;
                    total_rows = data.total_rows;
                }
                Err(
                    err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_)),
                ) => {
                    return map_client_err_to_response(err);
                }
                Err(_) => {}
            }
        }
    }

    let total_pages = if total_rows > 0 {
        (total_rows as usize).div_ceil(limit).max(1)
    } else if !data_rows.is_empty() {
        1
    } else {
        0
    };

    let template = TablesTemplate {
        databases: &databases,
        active_db,
        tables: &tables,
        active_table,
        active_tab,
        columns: &columns,
        data_columns: &data_columns,
        primary_keys: &primary_keys,
        data_rows: &data_rows,
        total_rows,
        page,
        limit,
        total_pages,
        initial_sql: params.sql.as_deref(),
        auth_enabled: true,
    };
    render_template_or_500(&template, "tables.html")
}

/// `GET /sql` -> Delegates to tables view with `tab=sql`.
pub async fn client_get_sql_console(
    state: State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
) -> Response {
    client_get_tables(
        state,
        headers,
        Query(ClientTablesQuery {
            tab: Some("sql".to_string()),
            ..Default::default()
        }),
    )
    .await
}

/// `GET /backups` -> Renders backups management view from sidecar `/api/backups`.
pub async fn client_get_backups(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let list: Vec<BasebackupMeta> = match state.sidecar_client.get_json("/api/backups", &token).await
    {
        Ok(v) => v,
        Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
            return map_client_err_to_response(err);
        }
        Err(_) => Vec::new(),
    };

    let total_bytes: u64 = list.iter().map(|b| b.total_bytes).sum();
    let latest_backup = list
        .first()
        .map(|b| b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let summary = BackupOverviewSummary {
        total_backups: list.len(),
        latest_backup,
        total_size_pretty: format_bytes(total_bytes),
        retention_days: state.retention_days,
        keep_count: state.keep_count,
        storage_endpoint: state.storage_endpoint.clone(),
        storage_bucket: state.storage_bucket.clone(),
    };

    let backup_items: Vec<BackupItemView> = list.iter().map(BackupItemView::from_meta).collect();
    let template = BackupsTemplate {
        backups: &backup_items,
        summary: &summary,
        auth_enabled: true,
    };
    render_template_or_500(&template, "backups.html")
}

/// `GET /users` -> Renders users & permissions management view from sidecar `/api/users`.
pub async fn client_get_users(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
    Query(params): Query<ClientUsersQuery>,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let roles: Vec<PgRole> = match state.sidecar_client.get_json("/api/users", &token).await {
        Ok(v) => v,
        Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
            return map_client_err_to_response(err);
        }
        Err(_) => Vec::new(),
    };

    let active_role_name = params
        .role
        .as_deref()
        .or_else(|| roles.first().map(|r| r.rolname.as_str()));
    let active_role = active_role_name.and_then(|name| roles.iter().find(|r| r.rolname == name));
    let active_tab = params.tab.as_deref().unwrap_or("attributes");

    let table_privileges: Vec<TablePrivilege> = if let Some(role) = active_role {
        let priv_path = format!("/api/users/{}/privileges", role.rolname);
        match state.sidecar_client.get_json(&priv_path, &token).await {
            Ok(v) => v,
            Err(
                err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_)),
            ) => {
                return map_client_err_to_response(err);
            }
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let all_tables: Vec<TableSummary> = match state.sidecar_client.get_json("/api/tables", &token).await
    {
        Ok(v) => v,
        Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
            return map_client_err_to_response(err);
        }
        Err(_) => Vec::new(),
    };

    let template = UsersTemplate {
        roles: &roles,
        active_role,
        active_tab,
        table_privileges: &table_privileges,
        all_tables: &all_tables,
        all_roles: &roles,
        auth_enabled: true,
    };
    render_template_or_500(&template, "users.html")
}

/// `GET /audit-logs` -> Renders audit log viewer from sidecar `/api/audit-logs`.
pub async fn client_get_audit_logs(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
    Query(query): Query<ClientAuditQuery>,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return Redirect::to("/login").into_response();
    };

    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;

    let mut pairs: Vec<(&str, String)> = vec![
        ("limit", limit.to_string()),
        ("offset", offset.to_string()),
    ];
    if let Some(ref k) = query.kind {
        pairs.push(("kind", k.clone()));
    }
    if let Some(ref q) = query.q {
        pairs.push(("q", q.clone()));
    }
    let qs = serde_urlencoded::to_string(&pairs).unwrap_or_default();
    let path = format!("/api/audit-logs?{qs}");

    let audit_resp: AuditListResponse = match state.sidecar_client.get_json(&path, &token).await {
        Ok(v) => v,
        Err(err) => return map_client_err_to_response(err),
    };

    let total = audit_resp.total;
    let total_pages = if total == 0 {
        1
    } else {
        total.div_ceil(limit)
    };
    let stats = AuditOverviewStats {
        total_events: total,
        dangerous_sql_count: audit_resp.dangerous_sql_count,
        latest_pitr_target: audit_resp.latest_pitr_target,
    };
    let views: Vec<AuditEventView> = audit_resp.events;
    let start_item = if total == 0 { 0 } else { offset + 1 };
    let end_item = (offset + views.len()).min(total);
    let start_page = page.saturating_sub(2).max(1);
    let end_page = (page + 2).min(total_pages);
    let page_items: Vec<PageItem> = (start_page..=end_page)
        .map(|num| PageItem {
            num,
            is_current: num == page,
        })
        .collect();

    let template = AuditTemplate {
        events: &views,
        stats: &stats,
        active_kind: query.kind.as_deref(),
        search_query: query.q.as_deref(),
        page,
        limit,
        total_pages,
        total_events: total,
        start_item,
        end_item,
        page_items,
        auth_enabled: true,
    };
    render_template_or_500(&template, "audit.html")
}
