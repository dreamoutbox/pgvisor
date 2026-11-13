use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use thiserror::Error;

/// Errors returned by the sidecar client-facing `/api/*` HTTP handlers.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Invalid sidecar port configuration: control_port and api_port cannot both be {0}")]
    PortCollision(u16),
}

impl ApiError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
            Self::Internal(_) | Self::PortCollision(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status_code, status_label) = self.status_and_code();
        (
            status_code,
            Json(serde_json::json!({
                "error": self.to_string(),
                "status": status_label,
            })),
        )
            .into_response()
    }
}
