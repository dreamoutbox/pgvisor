use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use pgvisor_core::pg_client::{PgClientError, PgConnection};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Outcome of verifying a PostgreSQL user's password against `pg_authid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordCheckOutcome {
    Verified,
    InvalidCredentials,
    BackendUnavailable(String),
}

/// Verifies username/password credentials against PostgreSQL authentication metadata.
#[async_trait::async_trait]
pub trait PasswordChecker: Send + Sync {
    /// Checks whether `username` can log in and `password` matches its stored verifier.
    async fn verify(&self, username: &str, password: &str) -> PasswordCheckOutcome;
}

/// Queries the local PostgreSQL instance over a trusted loopback connection and verifies
/// the user's SCRAM-SHA-256 verifier from `pg_authid`.
pub struct PostgresPasswordChecker {
    pg_addr: String,
    pg_superuser: String,
}

impl PostgresPasswordChecker {
    /// Creates a new checker targeting `pg_addr` as `pg_superuser`.
    pub fn new(pg_addr: impl Into<String>, pg_superuser: impl Into<String>) -> Self {
        Self {
            pg_addr: pg_addr.into(),
            pg_superuser: pg_superuser.into(),
        }
    }
}

#[async_trait::async_trait]
impl PasswordChecker for PostgresPasswordChecker {
    async fn verify(&self, username: &str, password: &str) -> PasswordCheckOutcome {
        if username.is_empty() || username.contains('\0') {
            return PasswordCheckOutcome::InvalidCredentials;
        }

        let escaped = username.replace('\'', "''");
        let sql = format!(
            "SELECT rolcanlogin, COALESCE(rolpassword, '') FROM pg_authid WHERE rolname = '{escaped}' LIMIT 1"
        );

        let result = match PgConnection::query_simple(
            &self.pg_addr,
            Some(&self.pg_superuser),
            Some("postgres"),
            &sql,
            1,
        )
        .await
        {
            Ok(res) => res,
            Err(PgClientError::SqlError(_)) => return PasswordCheckOutcome::InvalidCredentials,
            Err(e) => return PasswordCheckOutcome::BackendUnavailable(e.to_string()),
        };

        let Some(row) = result.rows.first() else {
            return PasswordCheckOutcome::InvalidCredentials;
        };
        if row.len() < 2 {
            return PasswordCheckOutcome::InvalidCredentials;
        }

        let can_login = row[0] == "t";
        let verifier = &row[1];
        if !can_login || verifier.is_empty() {
            return PasswordCheckOutcome::InvalidCredentials;
        }

        if verify_scram_sha256(password, verifier) {
            PasswordCheckOutcome::Verified
        } else {
            PasswordCheckOutcome::InvalidCredentials
        }
    }
}

/// Computes PBKDF2-HMAC-SHA256 (`Hi(password, salt, iterations)` per RFC 5802 / RFC 7677).
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Option<[u8; 32]> {
    if iterations == 0 {
        return None;
    }

    let mut mac = HmacSha256::new_from_slice(password).ok()?;
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = mac.finalize().into_bytes().into();
    let mut result = u;

    for _ in 1..iterations {
        let mut iter_mac = HmacSha256::new_from_slice(password).ok()?;
        iter_mac.update(&u);
        u = iter_mac.finalize().into_bytes().into();
        for (dst, src) in result.iter_mut().zip(u.iter()) {
            *dst ^= *src;
        }
    }

    Some(result)
}

/// Verifies a plaintext `password` against a PostgreSQL `SCRAM-SHA-256$iter:salt$stored:server` verifier.
///
/// # Arguments
/// * `password` - Candidate plaintext password.
/// * `verifier` - `pg_authid.rolpassword` string in `SCRAM-SHA-256` format.
pub fn verify_scram_sha256(password: &str, verifier: &str) -> bool {
    let Some(rest) = verifier.strip_prefix("SCRAM-SHA-256$") else {
        return false;
    };
    let Some((iter_and_salt, keys)) = rest.split_once('$') else {
        return false;
    };
    let Some((iter_str, salt_b64)) = iter_and_salt.split_once(':') else {
        return false;
    };
    let Some((stored_key_b64, server_key_b64)) = keys.split_once(':') else {
        return false;
    };

    let Ok(iterations) = iter_str.parse::<u32>() else {
        return false;
    };
    if iterations == 0 || iterations > 1_000_000 {
        return false;
    }

    let Ok(salt) = STANDARD.decode(salt_b64) else {
        return false;
    };
    let Ok(expected_stored_key) = STANDARD.decode(stored_key_b64) else {
        return false;
    };
    let Ok(expected_server_key) = STANDARD.decode(server_key_b64) else {
        return false;
    };
    if expected_stored_key.len() != 32 || expected_server_key.len() != 32 {
        return false;
    }

    let Some(salted_password) = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations) else {
        return false;
    };

    let Ok(mut client_mac) = HmacSha256::new_from_slice(&salted_password) else {
        return false;
    };
    client_mac.update(b"Client Key");
    let client_key = client_mac.finalize().into_bytes();
    let computed_stored_key = Sha256::digest(client_key);

    let Ok(mut server_mac) = HmacSha256::new_from_slice(&salted_password) else {
        return false;
    };
    server_mac.update(b"Server Key");
    let computed_server_key = server_mac.finalize().into_bytes();

    let stored_ok: bool = computed_stored_key
        .as_slice()
        .ct_eq(expected_stored_key.as_slice())
        .into();
    let server_ok: bool = computed_server_key
        .as_slice()
        .ct_eq(expected_server_key.as_slice())
        .into();

    stored_ok && server_ok
}
