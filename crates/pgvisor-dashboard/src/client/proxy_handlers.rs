use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::auth_handlers::extract_client_token;
use super::client_dashboard_state::ClientDashboardState;
use super::error::SidecarClientError;

fn to_reqwest_method(method: &Method) -> Option<reqwest::Method> {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).ok()
}

/// Reverse-proxies `/api/*` requests from the browser to the sidecar API cluster.
///
/// Read requests (`GET`/`HEAD` and read-only `/api/sql`) try any healthy sidecar first;
/// mutating requests target the cached leader and automatically retry on `409 NotLeader`.
pub async fn proxy_api_request(
    State(state): State<Arc<ClientDashboardState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = extract_client_token(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "status": "unauthorized",
                "error": "Missing authentication token",
            })),
        )
            .into_response();
    };

    let Some(req_method) = to_reqwest_method(&method) else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    };

    let path = uri.path();
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(path);
    let is_write = method != Method::GET && method != Method::HEAD && path != "/api/sql";

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());

    match state
        .sidecar_client
        .send_raw(
            is_write,
            req_method,
            path_and_query,
            Some(&token),
            content_type,
            body,
        )
        .await
    {
        Ok(upstream) => {
            let mut resp = (upstream.status, upstream.body).into_response();
            if let Some(ct) = upstream.content_type {
                if let Ok(val) = HeaderValue::from_str(&ct) {
                    resp.headers_mut().insert(header::CONTENT_TYPE, val);
                }
            }
            if let Some(cd) = upstream.content_disposition {
                if let Ok(val) = HeaderValue::from_str(&cd) {
                    resp.headers_mut().insert(header::CONTENT_DISPOSITION, val);
                }
            }
            resp
        }
        Err(SidecarClientError::Unauthorized) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "status": "unauthorized",
                "error": "Invalid or expired session token",
            })),
        )
            .into_response(),
        Err(SidecarClientError::Forbidden(msg)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "status": "forbidden",
                "error": msg,
            })),
        )
            .into_response(),
        Err(SidecarClientError::Unavailable(msg)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "status": "bad_gateway",
                "error": format!("Sidecar API unavailable: {msg}"),
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "status": "error",
                "error": err.to_string(),
            })),
        )
            .into_response(),
    }
}
