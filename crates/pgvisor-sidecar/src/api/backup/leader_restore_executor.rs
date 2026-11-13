use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use crate::api::ApiError;
use crate::control::{handle_restore, RestorePayload, SidecarState};

/// Abstraction over executing the local leader snapshot restore so tests can stub `pg_ctl`/shadow Postgres.
#[async_trait]
pub trait LeaderRestoreExecutor: Send + Sync {
    /// Applies `payload` to the local leader sidecar and returns the control response or mapped [`ApiError`].
    async fn execute_restore(
        &self,
        sidecar: &SidecarState,
        payload: RestorePayload,
    ) -> Result<(), ApiError>;

    /// Delay between standby `/control/resync` retry attempts during cluster restore.
    fn resync_retry_delay(&self) -> Duration {
        Duration::from_millis(1000)
    }
}

/// Production implementation invoking [`handle_restore`] on the local [`SidecarState`].
pub struct LocalLeaderRestoreExecutor;

#[async_trait]
impl LeaderRestoreExecutor for LocalLeaderRestoreExecutor {
    async fn execute_restore(
        &self,
        sidecar: &SidecarState,
        payload: RestorePayload,
    ) -> Result<(), ApiError> {
        // Success status is verified via `?`; response payload is only informational.
        let _resp = handle_restore(State(sidecar.clone()), Json(payload))
            .await
            .map_err(|(status, Json(body))| {
                let msg = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Leader restore failed")
                    .to_string();
                match status {
                    StatusCode::BAD_REQUEST => ApiError::BadRequest(msg),
                    StatusCode::NOT_FOUND => ApiError::NotFound(msg),
                    StatusCode::SERVICE_UNAVAILABLE => ApiError::ServiceUnavailable(msg),
                    _ => ApiError::Internal(msg),
                }
            })?;
        Ok(())
    }
}
