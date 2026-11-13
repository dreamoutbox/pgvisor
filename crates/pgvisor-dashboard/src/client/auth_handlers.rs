use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use serde::Deserialize;
use tracing::{error, warn};

use super::client_dashboard_state::ClientDashboardState;
use super::error::SidecarClientError;
use crate::templates::LoginTemplate;

const NON_SUPERUSER_MSG: &str = "user is not a PgVisor superuser";

#[derive(Debug, Default, Deserialize)]
pub struct LoginQuery {
    pub error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CredentialsPayload {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

fn build_session_cookie(token: &str, max_age_secs: u64) -> Option<HeaderValue> {
    let val = format!(
        "pgvisor_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}"
    );
    HeaderValue::from_str(&val).ok()
}

pub(crate) fn extract_client_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(tok) = auth.strip_prefix("Bearer ").map(str::trim) {
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
    }
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in cookie.split(';') {
            let trimmed = part.trim();
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim() == "pgvisor_token" && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn redirect_to_login_clearing_cookie() -> Response {
    let mut res = Redirect::to("/login").into_response();
    if let Some(cleared) = build_session_cookie("", 0) {
        res.headers_mut().insert(header::SET_COOKIE, cleared);
    }
    res
}

fn render_login_html(cluster_id: &str, status: StatusCode, error_msg: Option<&str>) -> Response {
    let template = LoginTemplate {
        cluster_id,
        error: error_msg,
    };
    match template.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(err) => {
            error!(?err, "Failed to render login template");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /login` -> Renders username + password login page for the standalone dashboard client.
pub async fn client_get_login_page(
    State(state): State<Arc<ClientDashboardState>>,
    Query(query): Query<LoginQuery>,
) -> Response {
    render_login_html(&state.cluster_id, StatusCode::OK, query.error.as_deref())
}

/// `POST /login` -> Authenticates PostgreSQL user credentials via sidecar `/api/auth/login` and sets cookie.
pub async fn client_post_login(
    State(state): State<Arc<ClientDashboardState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("application/json"));

    let creds = serde_json::from_slice::<CredentialsPayload>(&body).unwrap_or_else(|_| {
        serde_urlencoded::from_bytes::<CredentialsPayload>(&body).unwrap_or_default()
    });

    if creds.username.trim().is_empty() || creds.password.is_empty() {
        let msg = "Username and password are required";
        if is_json {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "status": "error", "message": msg })),
            )
                .into_response();
        }
        return render_login_html(&state.cluster_id, StatusCode::UNAUTHORIZED, Some(msg));
    }

    match state
        .sidecar_client
        .login(creds.username.trim(), &creds.password)
        .await
    {
        Ok(login_resp) => {
            let Some(cookie_header) = build_session_cookie(&login_resp.token, 86400) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };

            let mut res = if is_json {
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "status": "success",
                        "username": login_resp.username,
                        "message": "Authenticated successfully",
                    })),
                )
                    .into_response()
            } else {
                Redirect::to("/").into_response()
            };
            res.headers_mut().insert(header::SET_COOKIE, cookie_header);
            res
        }
        Err(SidecarClientError::Forbidden(_)) => {
            warn!(username = %creds.username, "Rejected login for non-superuser");
            if is_json {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "status": "forbidden",
                        "message": NON_SUPERUSER_MSG,
                    })),
                )
                    .into_response();
            }
            render_login_html(
                &state.cluster_id,
                StatusCode::FORBIDDEN,
                Some(NON_SUPERUSER_MSG),
            )
        }
        Err(SidecarClientError::Unauthorized) => {
            let msg = "Invalid username or password";
            if is_json {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "status": "error", "message": msg })),
                )
                    .into_response();
            }
            render_login_html(&state.cluster_id, StatusCode::UNAUTHORIZED, Some(msg))
        }
        Err(err) => {
            let msg = format!("Cluster sidecar unavailable: {err}");
            warn!(?err, "Login failed due to sidecar unavailability");
            if is_json {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "status": "error", "message": msg })),
                )
                    .into_response();
            }
            render_login_html(&state.cluster_id, StatusCode::BAD_GATEWAY, Some(&msg))
        }
    }
}

/// `GET /logout` or `POST /logout` -> Clears session cookie and redirects to `/login`.
pub async fn client_logout() -> Response {
    redirect_to_login_clearing_cookie()
}
