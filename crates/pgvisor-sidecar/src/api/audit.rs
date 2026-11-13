use axum::extract::{Query, State};
use axum::Json;
use pgvisor_core::api::{AuditEventView, AuditListResponse};
use pgvisor_core::audit::AuditEventKind;
use serde::Deserialize;

use super::api_state::ApiState;

const DEFAULT_AUDIT_LIMIT: usize = 50;
const MAX_AUDIT_LIMIT: usize = 200;

/// Query parameters for `GET /api/audit-logs`.
#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Handler for `GET /api/audit-logs`.
pub async fn get_audit_logs(
    State(state): State<ApiState>,
    Query(query): Query<AuditQuery>,
) -> Json<AuditListResponse> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_AUDIT_LIMIT)
        .clamp(1, MAX_AUDIT_LIMIT);
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

    let views = events
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
