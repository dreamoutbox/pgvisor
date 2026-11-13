use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pgvisor_core::api::{LoginResponse, MeResponse};
use pgvisor_core::audit::AuditLog;
use tokio::sync::RwLock;
use tower::ServiceExt;

use super::api_state::ApiState;
use super::password_verifier::{verify_scram_sha256, PasswordCheckOutcome, PasswordChecker};
use super::router::build_api_router;
use super::server::resolve_sidecar_ports;
use super::session_token::SessionTokenSigner;
use super::superusers::SuperuserAllowlist;
use crate::config::PostgresConfig;
use crate::control::{build_control_router, SidecarState};
use crate::supervisor::PostgresSupervisor;

struct MockPasswordChecker {
    verifiers: HashMap<String, String>,
}

#[async_trait::async_trait]
impl PasswordChecker for MockPasswordChecker {
    async fn verify(&self, username: &str, password: &str) -> PasswordCheckOutcome {
        match self.verifiers.get(username) {
            Some(verifier) if verify_scram_sha256(password, verifier) => {
                PasswordCheckOutcome::Verified
            }
            _ => PasswordCheckOutcome::InvalidCredentials,
        }
    }
}

// RFC 7677 Section 3 SCRAM-SHA-256 test vector for password "pencil"
const TEST_PENCIL_SCRAM: &str = "SCRAM-SHA-256$4096:QSXCR+Q6sek8bf92$FO+9jBb3MUukt6jJnzjPZOWc5ow/Pu6JtPyju0aqaE8=:qxJ1SbmSAi5EcS0J5Ck/cKAm/+Ixa+Kwp63f4OHDgzo=";

fn make_test_sidecar_state(secret: Option<String>) -> SidecarState {
    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        Arc::new(RwLock::new("leader".to_string())),
        "18.6".to_string(),
        Arc::new(Vec::new()),
        secret,
    )
}

fn make_test_api_state(allowlist_raw: Option<&str>, secret: Option<&str>) -> ApiState {
    let sidecar = make_test_sidecar_state(secret.map(ToString::to_string));
    let mut verifiers = HashMap::new();
    verifiers.insert("postgres".to_string(), TEST_PENCIL_SCRAM.to_string());
    verifiers.insert("alice".to_string(), TEST_PENCIL_SCRAM.to_string());

    ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(allowlist_raw, "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(secret)),
        Arc::new(AuditLog::new("test-cluster", None, 100)),
    )
    .with_password_checker(Arc::new(MockPasswordChecker { verifiers }))
}

#[test]
fn test_verify_scram_sha256_vector() {
    assert!(verify_scram_sha256("pencil", TEST_PENCIL_SCRAM));
    assert!(!verify_scram_sha256("wrong", TEST_PENCIL_SCRAM));
    assert!(!verify_scram_sha256("pencil", "md5abc123"));
    assert!(!verify_scram_sha256("pencil", "SCRAM-SHA-256$0:abc$def:ghi"));
}

#[test]
fn test_session_token_sign_verify_expiry_and_tamper() {
    let signer = SessionTokenSigner::from_cluster_secret(Some("shared-cluster-secret"));
    let (token, claims) = signer.sign_at("postgres", 1_000, 3_600).unwrap();
    assert_eq!(claims.username, "postgres");
    assert_eq!(claims.expires_at, 4_600);

    // Valid before expiry
    let verified = signer.verify_at(&token, 2_000).unwrap();
    assert_eq!(verified, claims);

    // Accepted by another sidecar with the same PGVISOR_CLUSTER_SECRET
    let peer_signer = SessionTokenSigner::from_cluster_secret(Some("shared-cluster-secret"));
    assert_eq!(peer_signer.verify_at(&token, 2_000).unwrap(), claims);

    // Rejected after expiry
    assert!(signer.verify_at(&token, 4_600).is_err());

    // Rejected if signature is tampered
    let mut bytes = token.clone().into_bytes();
    let last = bytes.len() - 1;
    bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert!(signer.verify_at(&tampered, 2_000).is_err());

    // Rejected if signed with a different secret
    let other_signer = SessionTokenSigner::from_cluster_secret(Some("other-secret"));
    assert!(other_signer.verify_at(&token, 2_000).is_err());
}

#[test]
fn test_superuser_allowlist_parsing() {
    let default_list = SuperuserAllowlist::parse(None, "postgres");
    assert!(default_list.is_allowed("postgres"));
    assert!(!default_list.is_allowed("alice"));
    assert_eq!(default_list.len(), 1);

    let blank_list = SuperuserAllowlist::parse(Some("  , , "), "pgadmin");
    assert!(blank_list.is_allowed("pgadmin"));
    assert_eq!(blank_list.len(), 1);

    let custom = SuperuserAllowlist::parse(Some(" postgres , bob, postgres , alice "), "postgres");
    assert!(custom.is_allowed("postgres"));
    assert!(custom.is_allowed("bob"));
    assert!(custom.is_allowed("alice"));
    assert!(!custom.is_allowed("mallory"));
    assert_eq!(custom.len(), 3);
}

#[test]
fn test_port_collision_rejected() {
    assert!(resolve_sidecar_ports(8080, 8080).is_err());
    assert_eq!(resolve_sidecar_ports(8080, 8081).unwrap(), (8080, 8081));
}

#[tokio::test]
async fn test_login_allowlist_and_password_checks() {
    let state = make_test_api_state(Some("postgres"), Some("cluster-secret"));
    let app = build_api_router(state.clone());

    // Valid Postgres user NOT in PGVISOR_SUPERUSER -> 403 Forbidden + audit log
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"alice","password":"pencil"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(state.audit_log.stats().await.0, 1);

    // Listed superuser with wrong password -> 401 Unauthorized
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"postgres","password":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Listed superuser with valid password -> 200 OK + token + HttpOnly cookie
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"postgres","password":"pencil"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("pgvisor_token="));
    assert!(set_cookie.contains("HttpOnly"));

    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let login: LoginResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(login.username, "postgres");
    assert!(!login.token.is_empty());
}

#[tokio::test]
async fn test_api_auth_middleware_enforcement() {
    let state = make_test_api_state(Some("postgres"), Some("cluster-secret"));
    let app = build_api_router(state.clone());

    // 1. Missing token -> 401
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Valid token for a user NOT in PGVISOR_SUPERUSER -> 403
    let (alice_token, _) = state.token_signer.sign("alice").unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header("authorization", format!("Bearer {alice_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 3. Valid token for superuser via Bearer header -> 200
    let (pg_token, _) = state.token_signer.sign("postgres").unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header("authorization", format!("Bearer {pg_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let me: MeResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(me.username, "postgres");

    // 4. Valid token via Cookie -> 200
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header("cookie", format!("pgvisor_token={pg_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_api_and_control_routers_are_isolated() {
    let secret = "cluster-secret";
    let api_state = make_test_api_state(Some("postgres"), Some(secret));
    let api_app = build_api_router(api_state.clone());
    let control_app = build_control_router(api_state.sidecar);

    // /control/* must not be reachable on the API router
    let resp = api_app
        .oneshot(
            Request::builder()
                .uri("/control/status")
                .header(
                    "authorization",
                    pgvisor_core::auth::make_auth_header_value(secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // /api/* must not be reachable on the control router
    let resp = control_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header(
                    "authorization",
                    pgvisor_core::auth::make_auth_header_value(secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
