use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use pgvisor_core::api::{
    ClusterMembersResponse, LoginResponse, MeResponse, MemberHealth, SidecarRole,
};
use pgvisor_core::audit::AuditLog;
use tokio::sync::RwLock;
use tower::ServiceExt;

use super::api_state::ApiState;
use super::error::ApiError;
use super::members::{build_api_url_from_peer, parse_node_id_from_peer, require_leader};
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

#[test]
fn test_peer_url_helpers() {
    assert_eq!(
        build_api_url_from_peer("http://pgvisor-node2:8080/", 8081),
        Some("http://pgvisor-node2:8081".to_string())
    );
    assert_eq!(
        build_api_url_from_peer("http://127.0.0.1:18080/control", 18081),
        Some("http://127.0.0.1:18081".to_string())
    );
    assert_eq!(parse_node_id_from_peer("http://pgvisor-node2:8080"), Some(2));
    assert_eq!(
        parse_node_id_from_peer("http://pgvisor-node10:8080"),
        Some(10)
    );
    assert_eq!(parse_node_id_from_peer("http://127.0.0.1:8080"), None);
}

async fn spawn_mock_control_peer(
    node_id: u64,
    role: &str,
    api_port: u16,
    secret: Option<&str>,
) -> (
    String,
    Arc<RwLock<String>>,
    tokio::task::JoinHandle<()>,
) {
    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let role_lock = Arc::new(RwLock::new(role.to_string()));
    let state = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        node_id,
        role_lock.clone(),
        "18.6".to_string(),
        Arc::new(Vec::new()),
        secret.map(ToString::to_string),
    )
    .with_api_port(api_port);

    let router = build_control_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), role_lock, handle)
}

#[tokio::test]
async fn test_cluster_members_fanout_healthy_unreachable_and_no_leader() {
    let secret = "shared-cluster-secret";
    let (peer2_url, peer2_role, peer2_task) =
        spawn_mock_control_peer(2, "leader", 18082, Some(secret)).await;

    // Reserve an unused port so the unreachable peer fails connection immediately.
    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unreachable_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let unreachable_url = format!("http://127.0.0.1:{unreachable_port}");

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let local_role = Arc::new(RwLock::new("standby".to_string()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        local_role,
        "18.6".to_string(),
        Arc::new(vec![peer2_url, unreachable_url]),
        Some(secret.to_string()),
    )
    .with_api_port(18081);

    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(Some(secret))),
        Arc::new(AuditLog::new("test-cluster", None, 100)),
    );
    let app = build_api_router(api_state.clone());
    let (token, _) = api_state.token_signer.sign("postgres").unwrap();

    // Case 1: healthy leader peer + local standby + unreachable peer
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/cluster/members")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let members_resp: ClusterMembersResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        members_resp.leader_url.as_deref(),
        Some("http://127.0.0.1:18082")
    );
    assert_eq!(members_resp.members.len(), 3);

    let self_member = members_resp.members.iter().find(|m| m.is_self).unwrap();
    assert_eq!(self_member.node_id, Some(1));
    assert_eq!(self_member.role, Some(SidecarRole::Standby));
    assert_eq!(self_member.health, MemberHealth::Healthy);
    assert_eq!(self_member.api_url.as_deref(), Some("http://127.0.0.1:18081"));

    let peer2_member = members_resp
        .members
        .iter()
        .find(|m| m.node_id == Some(2))
        .unwrap();
    assert!(!peer2_member.is_self);
    assert_eq!(peer2_member.role, Some(SidecarRole::Leader));
    assert_eq!(peer2_member.health, MemberHealth::Healthy);
    assert_eq!(peer2_member.api_url.as_deref(), Some("http://127.0.0.1:18082"));

    let dead_member = members_resp
        .members
        .iter()
        .find(|m| m.health == MemberHealth::Unreachable)
        .unwrap();
    assert_eq!(dead_member.role, None);
    assert_eq!(dead_member.api_url, None);
    assert!(!dead_member.is_self);

    // Case 2: peer 2 demoted -> no leader in cluster -> leader_url is None
    *peer2_role.write().await = "standby".to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster/members")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let no_leader_resp: ClusterMembersResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(no_leader_resp.leader_url, None);

    peer2_task.abort();
}

#[tokio::test]
async fn test_require_leader_and_409_response() {
    let secret = "shared-cluster-secret";
    let (peer2_url, _peer2_role, peer2_task) =
        spawn_mock_control_peer(2, "leader", 18082, Some(secret)).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let local_role = Arc::new(RwLock::new("standby".to_string()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        local_role.clone(),
        "18.6".to_string(),
        Arc::new(vec![peer2_url]),
        Some(secret.to_string()),
    )
    .with_api_port(18081);

    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(Some(secret))),
        Arc::new(AuditLog::new("test-cluster", None, 100)),
    );

    let err = require_leader(&api_state).await.unwrap_err();
    match &err {
        ApiError::NotLeader { leader_url } => {
            assert_eq!(leader_url.as_deref(), Some("http://127.0.0.1:18082"));
        }
        other => panic!("Expected NotLeader error, got {other:?}"),
    }

    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "not_leader");
    assert_eq!(json["leader_url"], "http://127.0.0.1:18082");

    // Promoted local sidecar passes require_leader
    *local_role.write().await = "leader".to_string();
    assert!(require_leader(&api_state).await.is_ok());

    peer2_task.abort();
}
