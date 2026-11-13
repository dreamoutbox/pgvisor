use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use pgvisor_core::api::{LoginRequest, LoginResponse, MeResponse};
use pgvisor_core::audit::AuditEventKind;

use super::api_state::ApiState;
use super::authenticated_user::AuthenticatedUser;
use super::error::ApiError;
use super::password_verifier::PasswordCheckOutcome;
use super::session_token::DEFAULT_TOKEN_TTL_SECS;

pub const SESSION_COOKIE_NAME: &str = "pgvisor_token";

/// Extracts a session token from `Authorization: Bearer <token>` or `Cookie: pgvisor_token=<token>`.
pub fn extract_token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(auth_val) = headers.get(AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            let trimmed = auth_str.trim();
            if trimmed.len() > 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
                let token = trimmed[7..].trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }

    if let Some(cookie_val) = headers.get(COOKIE) {
        if let Ok(cookie_str) = cookie_val.to_str() {
            for pair in cookie_str.split(';') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k.trim() == SESSION_COOKIE_NAME {
                        let val = v.trim();
                        if !val.is_empty() {
                            return Some(val.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

/// `POST /api/auth/login` -> Authenticates a PostgreSQL user listed in `PGVISOR_SUPERUSER`.
pub async fn post_login(
    State(state): State<ApiState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let username = payload.username.trim();
    if username.is_empty() {
        return Err(ApiError::BadRequest("Username is required".into()));
    }

    // Check PGVISOR_SUPERUSER allowlist BEFORE verifying the password against Postgres.
    if !state.superusers.is_allowed(username) {
        let detail = format!(
            "API login denied for '{}': user is not in PGVISOR_SUPERUSER",
            username
        );
        state
            .audit_log
            .append(
                AuditEventKind::UserPermission,
                Some(state.sidecar.node_id),
                Some(&state.pg_addr),
                detail,
                None,
            )
            .await;
        return Err(ApiError::Forbidden(
            "User is not authorized in PGVISOR_SUPERUSER".into(),
        ));
    }

    match state
        .password_checker
        .verify(username, &payload.password)
        .await
    {
        PasswordCheckOutcome::Verified => {}
        PasswordCheckOutcome::InvalidCredentials => {
            return Err(ApiError::Unauthorized(
                "Invalid PostgreSQL username or password".into(),
            ));
        }
        PasswordCheckOutcome::BackendUnavailable(reason) => {
            return Err(ApiError::ServiceUnavailable(format!(
                "Cannot verify credentials with local PostgreSQL: {reason}"
            )));
        }
    }

    let (token, claims) = state.token_signer.sign(username)?;
    let cookie_val = format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={DEFAULT_TOKEN_TTL_SECS}"
    );

    let mut response = (
        StatusCode::OK,
        Json(LoginResponse {
            token,
            username: claims.username,
            expires_at: claims.expires_at,
        }),
    )
        .into_response();

    if let Ok(header_val) = HeaderValue::from_str(&cookie_val) {
        response.headers_mut().insert(SET_COOKIE, header_val);
    }

    Ok(response)
}

/// `GET /api/auth/me` -> Returns the authenticated superuser's username.
pub async fn get_me(Extension(user): Extension<AuthenticatedUser>) -> Json<MeResponse> {
    Json(MeResponse {
        username: user.username,
    })
}

/// Enforces valid session token + `PGVISOR_SUPERUSER` membership on every protected `/api/*` request.
pub async fn api_auth_middleware(
    State(state): State<ApiState>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(token) = extract_token_from_headers(req.headers()) else {
        return ApiError::Unauthorized("Missing session token".into()).into_response();
    };

    let claims = match state.token_signer.verify(&token) {
        Ok(claims) => claims,
        Err(err) => return err.into_response(),
    };

    // Re-check PGVISOR_SUPERUSER on every request so allowlist changes or mismatched node configs are rejected.
    if !state.superusers.is_allowed(&claims.username) {
        return ApiError::Forbidden("User is not authorized in PGVISOR_SUPERUSER".into())
            .into_response();
    }

    req.extensions_mut().insert(AuthenticatedUser {
        username: claims.username,
    });

    next.run(req).await
}
