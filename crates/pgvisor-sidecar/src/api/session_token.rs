use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::warn;
use uuid::Uuid;

use super::error::ApiError;

type HmacSha256 = Hmac<Sha256>;

/// Default lifetime for issued session tokens (24 hours).
pub const DEFAULT_TOKEN_TTL_SECS: i64 = 86_400;

/// Allowed clock skew when validating token `issued_at`.
const MAX_CLOCK_SKEW_SECS: i64 = 60;

/// Stateless session claims embedded inside a signed sidecar API token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
    pub username: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Signs and verifies stateless HMAC-SHA256 session tokens across cluster sidecars.
pub struct SessionTokenSigner {
    key: Vec<u8>,
}

impl SessionTokenSigner {
    /// Builds a signer using `PGVISOR_CLUSTER_SECRET` when present, or an ephemeral random key.
    ///
    /// # Arguments
    /// * `cluster_secret` - Optional cluster-wide shared secret.
    pub fn from_cluster_secret(cluster_secret: Option<&str>) -> Self {
        if let Some(secret) = cluster_secret.map(str::trim).filter(|s| !s.is_empty()) {
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .expect("HMAC-SHA256 accepts keys of any length");
            mac.update(b"pgvisor-api-session-v1");
            let derived = mac.finalize().into_bytes().to_vec();
            Self { key: derived }
        } else {
            warn!(
                "PGVISOR_CLUSTER_SECRET is not configured; generating ephemeral API session signing key (tokens will not validate across sidecars)"
            );
            let mut key = Vec::with_capacity(32);
            key.extend_from_slice(Uuid::new_v4().as_bytes());
            key.extend_from_slice(Uuid::new_v4().as_bytes());
            Self { key }
        }
    }

    /// Creates a signer from an explicit signing key (primarily for tests).
    pub fn from_raw_key(key: Vec<u8>) -> Self {
        Self { key }
    }

    /// Signs a session token for `username` at `now_unix` with the given `ttl_secs`.
    pub fn sign_at(
        &self,
        username: &str,
        now_unix: i64,
        ttl_secs: i64,
    ) -> Result<(String, SessionClaims), ApiError> {
        let claims = SessionClaims {
            username: username.to_string(),
            issued_at: now_unix,
            expires_at: now_unix.saturating_add(ttl_secs),
        };
        let payload_json = serde_json::to_vec(&claims)
            .map_err(|e| ApiError::Internal(format!("Failed to serialize session claims: {e}")))?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload_json);

        let mut mac = HmacSha256::new_from_slice(&self.key)
            .map_err(|e| ApiError::Internal(format!("HMAC initialization failed: {e}")))?;
        mac.update(payload_b64.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());

        Ok((format!("{payload_b64}.{sig_b64}"), claims))
    }

    /// Signs a session token using the current UTC timestamp and [`DEFAULT_TOKEN_TTL_SECS`].
    pub fn sign(&self, username: &str) -> Result<(String, SessionClaims), ApiError> {
        let now = chrono::Utc::now().timestamp();
        self.sign_at(username, now, DEFAULT_TOKEN_TTL_SECS)
    }

    /// Verifies token signature and expiry at `now_unix`.
    pub fn verify_at(&self, token: &str, now_unix: i64) -> Result<SessionClaims, ApiError> {
        let Some((payload_b64, sig_b64)) = token.trim().split_once('.') else {
            return Err(ApiError::Unauthorized("Malformed session token".into()));
        };
        if payload_b64.is_empty() || sig_b64.is_empty() || sig_b64.contains('.') {
            return Err(ApiError::Unauthorized("Malformed session token".into()));
        }

        let sig_bytes = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| ApiError::Unauthorized("Invalid session token signature encoding".into()))?;

        let mut mac = HmacSha256::new_from_slice(&self.key)
            .map_err(|e| ApiError::Internal(format!("HMAC initialization failed: {e}")))?;
        mac.update(payload_b64.as_bytes());
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ApiError::Unauthorized("Invalid session token signature".into()))?;

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| ApiError::Unauthorized("Invalid session token payload".into()))?;
        let claims: SessionClaims = serde_json::from_slice(&payload_bytes)
            .map_err(|_| ApiError::Unauthorized("Invalid session token claims".into()))?;

        if claims.username.trim().is_empty() {
            return Err(ApiError::Unauthorized("Session token has empty username".into()));
        }
        if claims.expires_at <= now_unix {
            return Err(ApiError::Unauthorized("Session token has expired".into()));
        }
        if claims.issued_at > now_unix + MAX_CLOCK_SKEW_SECS {
            return Err(ApiError::Unauthorized("Session token issued in the future".into()));
        }

        Ok(claims)
    }

    /// Verifies token signature and expiry against the current UTC timestamp.
    pub fn verify(&self, token: &str) -> Result<SessionClaims, ApiError> {
        self.verify_at(token, chrono::Utc::now().timestamp())
    }
}
