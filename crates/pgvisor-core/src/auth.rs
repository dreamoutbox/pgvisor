use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub const CLUSTER_SECRET_ENV: &str = "PGVISOR_CLUSTER_SECRET";
pub const AUTH_PAYLOAD: &[u8] = b"pgvisor-internal-v1";
pub const BEARER_PREFIX: &str = "Bearer ";

/// Reads and trims the cluster secret from the environment.
pub fn cluster_secret_from_env() -> Option<String> {
    std::env::var(CLUSTER_SECRET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Derives a deterministic HMAC-SHA256 bearer token for the given cluster secret.
pub fn derive_bearer_token(secret: &str) -> String {
    // HMAC-SHA256 accepts keys of arbitrary byte length without failure
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 key slice initialization is infallible");
    mac.update(AUTH_PAYLOAD);
    hex::encode(mac.finalize().into_bytes())
}

/// Formats the Authorization HTTP header value ("Bearer <token>").
pub fn make_auth_header_value(secret: &str) -> String {
    format!("{}{}", BEARER_PREFIX, derive_bearer_token(secret))
}

/// Validates an incoming Authorization header value against the expected cluster secret using constant-time comparison.
pub fn validate_bearer_token(secret: &str, header_val: &str) -> bool {
    let header_val = header_val.trim();
    let provided_token = if header_val.len() > 7 && header_val[..7].eq_ignore_ascii_case(BEARER_PREFIX) {
        header_val[7..].trim()
    } else {
        return false;
    };

    let expected_token = derive_bearer_token(secret);

    // Constant-time comparison to protect against timing attacks
    if provided_token.len() != expected_token.len() {
        return false;
    }

    provided_token.as_bytes().ct_eq(expected_token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_bearer_token_deterministic() {
        let token1 = derive_bearer_token("my-cluster-password-123");
        let token2 = derive_bearer_token("my-cluster-password-123");
        assert_eq!(token1, token2);
        assert_eq!(token1.len(), 64);
    }

    #[test]
    fn test_derive_bearer_token_different_secrets() {
        let token1 = derive_bearer_token("secret-a");
        let token2 = derive_bearer_token("secret-b");
        assert_ne!(token1, token2);
    }

    #[test]
    fn test_make_auth_header_value() {
        let secret = "test-secret";
        let header = make_auth_header_value(secret);
        assert!(header.starts_with("Bearer "));
        assert_eq!(header[7..], derive_bearer_token(secret));
    }

    #[test]
    fn test_validate_bearer_token_valid() {
        let secret = "super-secret-key";
        let header = make_auth_header_value(secret);
        assert!(validate_bearer_token(secret, &header));

        // Test with lowercase bearer prefix
        let lowercase_header = format!("bearer {}", derive_bearer_token(secret));
        assert!(validate_bearer_token(secret, &lowercase_header));
    }

    #[test]
    fn test_validate_bearer_token_invalid() {
        let secret = "super-secret-key";
        let header = make_auth_header_value(secret);

        // Wrong secret
        assert!(!validate_bearer_token("wrong-secret", &header));

        // Missing Bearer prefix
        assert!(!validate_bearer_token(secret, &derive_bearer_token(secret)));

        // Malformed or truncated token
        assert!(!validate_bearer_token(secret, "Bearer abc"));
        assert!(!validate_bearer_token(secret, ""));
        assert!(!validate_bearer_token(secret, "Basic dXNlcjpwYXNz"));
    }
}
