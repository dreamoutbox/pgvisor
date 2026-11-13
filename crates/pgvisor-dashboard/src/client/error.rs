use axum::http::StatusCode;
use thiserror::Error;

/// Errors raised when communicating with upstream `pgvisor-sidecar` API nodes.
#[derive(Debug, Error)]
pub enum SidecarClientError {
    #[error("No sidecar API URLs configured")]
    EmptyUrls,

    #[error("Unauthorized: invalid or expired session token")]
    Unauthorized,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Upstream sidecar error ({status}): {message}")]
    Upstream { status: StatusCode, message: String },

    #[error("All configured sidecar API endpoints are unavailable: {0}")]
    Unavailable(String),

    #[error("Invalid response payload from sidecar: {0}")]
    Decode(String),
}
