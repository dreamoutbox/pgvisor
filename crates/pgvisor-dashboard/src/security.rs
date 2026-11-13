use thiserror::Error;

/// Errors returned by admin token validation in the legacy dashboard.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdminAuthError {
    #[error("Unauthorized: invalid or missing admin token")]
    Unauthorized,
}

/// Admin token authentication helpers for the dashboard.
pub struct AdminTokenGuard;

impl AdminTokenGuard {
    /// Verifies admin authentication token.
    pub fn verify_admin_token(
        auth_header: Option<&str>,
        expected_token: &str,
    ) -> Result<(), AdminAuthError> {
        match auth_header {
            Some(header) => {
                let token = if let Some(stripped) = header.strip_prefix("Bearer ") {
                    stripped
                } else {
                    header
                };
                if token.trim() == expected_token.trim() {
                    Ok(())
                } else {
                    Err(AdminAuthError::Unauthorized)
                }
            }
            None => Err(AdminAuthError::Unauthorized),
        }
    }

    /// Extracts an admin token from HTTP request headers.
    /// Supports Authorization: Bearer <token>, X-Admin-Token: <token>,
    /// and Cookie: pgvisor_token=<token>.
    pub fn extract_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
        // 1. Authorization: Bearer <token>
        if let Some(auth_val) = headers.get(axum::http::header::AUTHORIZATION) {
            if let Ok(auth_str) = auth_val.to_str() {
                let trimmed = auth_str.trim();
                if let Some(bearer) = trimmed.strip_prefix("Bearer ") {
                    let token = bearer.trim();
                    if !token.is_empty() {
                        return Some(token.to_string());
                    }
                } else if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 2. X-Admin-Token: <token>
        if let Some(custom_val) = headers.get("x-admin-token") {
            if let Ok(custom_str) = custom_val.to_str() {
                let trimmed = custom_str.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 3. Cookie: pgvisor_token=<token>
        if let Some(cookie_val) = headers.get(axum::http::header::COOKIE) {
            if let Ok(cookie_str) = cookie_val.to_str() {
                for pair in cookie_str.split(';') {
                    let mut parts = pair.splitn(2, '=');
                    if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                        if k.trim() == "pgvisor_token" {
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
}

#[cfg(test)]
mod tests {
    use super::AdminTokenGuard;

    #[test]
    fn test_auth_token_verification() {
        let expected = "secret-pgvisor-token";
        assert!(AdminTokenGuard::verify_admin_token(
            Some("Bearer secret-pgvisor-token"),
            expected
        )
        .is_ok());
        assert!(
            AdminTokenGuard::verify_admin_token(Some("secret-pgvisor-token"), expected).is_ok()
        );
        assert!(AdminTokenGuard::verify_admin_token(Some("wrong-token"), expected).is_err());
        assert!(AdminTokenGuard::verify_admin_token(None, expected).is_err());
    }

    #[test]
    fn test_extract_token_from_headers() {
        use axum::http::header::{AUTHORIZATION, COOKIE};
        use axum::http::HeaderMap;

        // Bearer token
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer my-secret-token".parse().unwrap());
        assert_eq!(
            AdminTokenGuard::extract_token_from_headers(&headers),
            Some("my-secret-token".to_string())
        );

        // Custom X-Admin-Token
        let mut headers = HeaderMap::new();
        headers.insert("x-admin-token", "custom-token-xyz".parse().unwrap());
        assert_eq!(
            AdminTokenGuard::extract_token_from_headers(&headers),
            Some("custom-token-xyz".to_string())
        );

        // Cookie pgvisor_token
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            "other_val=123; pgvisor_token=cookie-secret; foo=bar"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            AdminTokenGuard::extract_token_from_headers(&headers),
            Some("cookie-secret".to_string())
        );

        // Empty / none
        let headers = HeaderMap::new();
        assert_eq!(AdminTokenGuard::extract_token_from_headers(&headers), None);
    }
}
