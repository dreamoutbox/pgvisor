use std::sync::Arc;

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use serde::Deserialize;
use tracing::error;

use super::state::DashboardState;
use crate::security::SqlSecurityGuard;
use crate::templates::LoginTemplate;

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub token: String,
}

/// GET /login -> Renders dashboard login page
pub async fn get_login_page(
    State(state): State<Arc<DashboardState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let expected_token = match state.admin_token.as_deref() {
        Some(tok) => tok,
        None => return Redirect::to("/").into_response(),
    };

    // If already authenticated via cookie or header, redirect directly to /
    if let Some(token) = SqlSecurityGuard::extract_token_from_headers(&headers) {
        if token == expected_token {
            return Redirect::to("/").into_response();
        }
    }

    let overview = state.overview.read().await;
    let template = LoginTemplate {
        cluster_id: &overview.cluster_id,
        error: None,
    };
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!(?e, "Failed to render login template");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /login -> Validates admin token and sets session cookie
pub async fn post_login(
    State(state): State<Arc<DashboardState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let expected_token = match state.admin_token.as_deref() {
        Some(tok) => tok,
        None => return Redirect::to("/").into_response(),
    };

    let submitted_token = if let Ok(json) = serde_json::from_slice::<LoginForm>(&body) {
        json.token
    } else if let Ok(form) = serde_urlencoded::from_bytes::<LoginForm>(&body) {
        form.token
    } else {
        String::new()
    };

    let is_json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("application/json"))
        .unwrap_or(false);

    if submitted_token.trim() == expected_token.trim() {
        let cookie_val = format!(
            "pgvisor_token={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400",
            submitted_token.trim()
        );
        let header_val = match axum::http::HeaderValue::from_str(&cookie_val) {
            Ok(v) => v,
            Err(e) => {
                error!(?e, "Invalid cookie header value");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

        if is_json {
            let mut res = (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "success",
                    "message": "Authenticated successfully"
                })),
            )
                .into_response();
            res.headers_mut()
                .insert(axum::http::header::SET_COOKIE, header_val);
            res
        } else {
            let mut res = Redirect::to("/").into_response();
            *res.status_mut() = StatusCode::SEE_OTHER;
            res.headers_mut()
                .insert(axum::http::header::SET_COOKIE, header_val);
            res
        }
    } else if is_json {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Invalid admin token",
                "status": "unauthorized"
            })),
        )
            .into_response()
    } else {
        let overview = state.overview.read().await;
        let template = LoginTemplate {
            cluster_id: &overview.cluster_id,
            error: Some("Invalid admin authentication token"),
        };
        match template.render() {
            Ok(html) => (StatusCode::UNAUTHORIZED, Html(html)).into_response(),
            Err(e) => {
                error!(?e, "Failed to render login template");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// GET /logout or POST /logout -> Clears authentication cookie and redirects to /login
pub async fn get_logout() -> Response {
    let mut res = Redirect::to("/login").into_response();
    *res.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(val) = axum::http::HeaderValue::from_str(
        "pgvisor_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
    ) {
        res.headers_mut()
            .insert(axum::http::header::SET_COOKIE, val);
    }
    res
}
