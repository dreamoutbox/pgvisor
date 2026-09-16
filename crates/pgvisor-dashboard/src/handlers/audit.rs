use std::sync::Arc;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use pgvisor_core::audit::AuditEventKind;
use serde::Deserialize;

use super::state::DashboardState;
use crate::models::{AuditEventView, AuditListResponse, AuditOverviewStats};
use crate::templates::{AuditTemplate, PageItem};

/// Query parameters for listing and searching audit events.
#[derive(Deserialize, Default)]
pub struct AuditQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /audit-logs -> Renders Audit Logs HTML dashboard page
pub async fn get_audit_logs_page(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<AuditQuery>,
) -> Result<Html<String>, StatusCode> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;

    let target_kind = query
        .kind
        .as_deref()
        .and_then(AuditEventKind::from_snake_case);
    let (events, total) = state
        .audit_log
        .list(limit, offset, target_kind, query.q.as_deref())
        .await;

    let total_pages = if total == 0 {
        1
    } else {
        (total + limit - 1) / limit
    };

    let (total_events, dangerous_sql_count, latest_pitr_target) = state.audit_log.stats().await;
    let stats = AuditOverviewStats {
        total_events,
        dangerous_sql_count,
        latest_pitr_target,
    };

    let views: Vec<AuditEventView> = events
        .into_iter()
        .map(|e| AuditEventView {
            id: e.id,
            occurred_at: e.occurred_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            kind: e.kind.as_str().to_string(),
            kind_display: e.kind.display_name().to_string(),
            node_id: e.node_id,
            node_address: e.node_address,
            detail: e.detail,
            pitr_target: e.pitr_target,
        })
        .collect();

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
        auth_enabled: state.admin_token.is_some(),
    };

    template
        .render()
        .map(Html)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// GET /api/audit-logs -> Returns paginated JSON audit events for monitoring and automated verification
pub async fn api_list_audit_logs(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<AuditQuery>,
) -> Json<AuditListResponse> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or_else(|| {
        let p = query.page.unwrap_or(1).max(1);
        (p - 1) * limit
    });

    let target_kind = query
        .kind
        .as_deref()
        .and_then(AuditEventKind::from_snake_case);
    let (events, total) = state
        .audit_log
        .list(limit, offset, target_kind, query.q.as_deref())
        .await;

    let (_total_events, dangerous_sql_count, latest_pitr_target) = state.audit_log.stats().await;

    let views: Vec<AuditEventView> = events
        .into_iter()
        .map(|e| AuditEventView {
            id: e.id,
            occurred_at: e.occurred_at.to_rfc3339(),
            kind: e.kind.as_str().to_string(),
            kind_display: e.kind.display_name().to_string(),
            node_id: e.node_id,
            node_address: e.node_address,
            detail: e.detail,
            pitr_target: e.pitr_target,
        })
        .collect();

    Json(AuditListResponse {
        events: views,
        total,
        limit,
        offset,
        dangerous_sql_count,
        latest_pitr_target,
    })
}
