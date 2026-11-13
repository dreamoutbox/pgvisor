use axum::body::Bytes;
use axum::http::StatusCode;

/// Raw HTTP response returned from an upstream sidecar `/api/*` request.
#[derive(Debug, Clone)]
pub struct SidecarResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    pub body: Bytes,
}
