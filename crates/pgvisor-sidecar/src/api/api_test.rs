use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pgvisor_core::api::{
    AlterRoleRequest, AuditListResponse, ClusterMembersResponse, ClusterMetricsSnapshot,
    ClusterOverview, ColumnInfo, CreateRoleRequest, DeleteRowRequest, LoginResponse, MeResponse,
    MemberHealth, NodeConfigResponse, NodeLogsResponse, NodeMetricRole, NodeMetrics, NodeSummary,
    PgNodeActivity, PgRole, RoleMembershipRequest, RowMutationResponse, SidecarRole, SqlQueryError,
    SqlQueryRequest, SqlQueryResult, SwitchoverRequest, SwitchoverResponse, TableDataResponse,
    TablePrivilege, TablePrivilegeKind, TablePrivilegeRequest, TableSummary, UpdateRowRequest,
};
use pgvisor_core::audit::{AuditEventKind, AuditLog};
use pgvisor_core::sql_guard::SqlSecurityGuard;
use tokio::sync::{Mutex, RwLock};
use tower::ServiceExt;

use super::api_state::ApiState;
use super::error::ApiError;
use super::members::{build_api_url_from_peer, parse_node_id_from_peer, require_leader};
use super::metrics::build_metrics_snapshot;
use super::password_verifier::{verify_scram_sha256, PasswordCheckOutcome, PasswordChecker};
use super::pg_activity::{sum_cluster_activity, sum_pg_stat_database_rows, ActivityTracker};
use super::router::build_api_router;
use super::server::resolve_sidecar_ports;
use super::session_token::SessionTokenSigner;
use super::sql_runner::ApiSqlRunner;
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

type RecordedQueryCall = (String, Option<String>, String);

#[derive(Default)]
struct RecordingSqlRunner {
    calls: Arc<Mutex<Vec<RecordedQueryCall>>>,
}

#[async_trait::async_trait]
impl ApiSqlRunner for RecordingSqlRunner {
    async fn execute_as_user(
        &self,
        username: &str,
        database: Option<&str>,
        sql: &str,
        max_rows: usize,
    ) -> Result<SqlQueryResult, ApiError> {
        self.calls.lock().await.push((
            username.to_string(),
            database.map(ToString::to_string),
            sql.to_string(),
        ));
        let upper = sql.to_ascii_uppercase();
        let (columns, rows) = if upper.contains("PG_DATABASE") {
            (
                vec!["datname".into()],
                vec![vec!["postgres".into()], vec!["app_db".into()]],
            )
        } else if upper.contains("INFORMATION_SCHEMA.TABLES") && upper.contains("HAS_TABLE_PRIVILEGE")
        {
            (
                vec![
                    "table_name".into(),
                    "can_select".into(),
                    "can_insert".into(),
                    "can_update".into(),
                    "can_delete".into(),
                    "can_truncate".into(),
                    "can_references".into(),
                    "can_trigger".into(),
                ],
                vec![vec![
                    "pgvisor_demo".into(),
                    "t".into(),
                    "t".into(),
                    "f".into(),
                    "f".into(),
                    "f".into(),
                    "f".into(),
                    "f".into(),
                ]],
            )
        } else if upper.contains("INFORMATION_SCHEMA.TABLES") {
            (
                vec!["table_name".into()],
                vec![vec!["pgvisor_demo".into()]],
            )
        } else if upper.contains("KEY_COLUMN_USAGE") {
            (vec!["column_name".into()], vec![vec!["id".into()]])
        } else if upper.contains("INFORMATION_SCHEMA.COLUMNS") {
            (
                vec![
                    "column_name".into(),
                    "data_type".into(),
                    "is_nullable".into(),
                    "column_default".into(),
                ],
                vec![
                    vec!["id".into(), "integer".into(), "NO".into(), "NULL".into()],
                    vec!["name".into(), "text".into(), "YES".into(), "NULL".into()],
                ],
            )
        } else if upper.contains("COUNT(*)") {
            (vec!["count".into()], vec![vec!["2".into()]])
        } else if upper.contains("FROM PG_ROLES") {
            (
                vec![
                    "rolname".into(),
                    "rolcanlogin".into(),
                    "rolcreatedb".into(),
                    "rolcreaterole".into(),
                    "rolreplication".into(),
                    "rolsuper".into(),
                    "rolconnlimit".into(),
                    "member_of".into(),
                ],
                vec![vec![
                    "app_user".into(),
                    "t".into(),
                    "f".into(),
                    "f".into(),
                    "f".into(),
                    "f".into(),
                    "25".into(),
                    "readers".into(),
                ]],
            )
        } else if upper.starts_with("SELECT * FROM") {
            (
                vec!["id".into(), "name".into()],
                vec![
                    vec!["1".into(), "alpha".into()],
                    vec!["2".into(), "beta".into()],
                ],
            )
        } else {
            (
                vec!["ok".into()],
                vec![vec![format!("executed:{username}")]],
            )
        };
        let row_count = rows.len().min(max_rows);
        Ok(SqlQueryResult {
            columns,
            rows: rows.into_iter().take(row_count).collect(),
            execution_time_ms: 1,
            row_count,
            truncated: false,
        })
    }
}

#[test]
fn test_activity_sampler_and_counter_reset() {
    let cols: Vec<String> = vec![
        "xact_commit",
        "xact_rollback",
        "tup_returned",
        "tup_fetched",
        "tup_inserted",
        "tup_updated",
        "tup_deleted",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect();

    let rows = vec![
        vec!["10", "1", "100", "25", "5", "2", "1"]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        vec!["20", "2", "300", "75", "10", "3", "0"]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    ];

    let parsed = sum_pg_stat_database_rows(&cols, &rows).unwrap();
    assert_eq!(parsed.xact_commit, 30);
    assert_eq!(parsed.xact_rollback, 3);
    assert_eq!(parsed.reads(), 500);
    assert_eq!(parsed.writes(), 21);

    let mut tracker = ActivityTracker::default();
    assert_eq!(tracker.observe(1, Some(parsed)), (500, 21));

    // Counter temporarily unavailable preserves last cumulative counts.
    assert_eq!(tracker.observe(1, None), (500, 21));

    // Counter reset (e.g. pg_stat_reset() / restart) preserves prior total + adds post-reset reads/writes.
    let post_reset = PgNodeActivity {
        tup_returned: 30,
        tup_fetched: 10,
        tup_inserted: 4,
        ..Default::default()
    };
    assert_eq!(tracker.observe(1, Some(post_reset)), (540, 25));

    let node2_act = PgNodeActivity {
        tup_returned: 200,
        tup_fetched: 60,
        tup_inserted: 1,
        ..Default::default()
    };
    assert_eq!(tracker.observe(2, Some(node2_act)), (260, 1));

    let cluster = sum_cluster_activity(&[
        NodeMetrics {
            node_id: 1,
            address: "n1:5432".into(),
            role: NodeMetricRole::Leader,
            uptime_secs: 10,
            replication_lag_bytes: 0,
            cpu_percent: 1.0,
            memory_used_bytes: 0,
            memory_total_bytes: 0,
            disk_used_bytes: 0,
            pg_reads: 540,
            pg_writes: 25,
        },
        NodeMetrics {
            node_id: 2,
            address: "n2:5432".into(),
            role: NodeMetricRole::Standby,
            uptime_secs: 10,
            replication_lag_bytes: 0,
            cpu_percent: 1.0,
            memory_used_bytes: 0,
            memory_total_bytes: 0,
            disk_used_bytes: 0,
            pg_reads: 260,
            pg_writes: 1,
        },
    ]);
    assert_eq!(cluster.reads_total, 800);
    assert_eq!(cluster.writes_total, 26);
}

#[tokio::test]
async fn test_metrics_snapshot_and_60_sample_ring() {
    let secret = "cluster-secret";
    let (peer2_url, _peer2_role, peer2_task) =
        spawn_mock_control_peer(2, "standby", 18082, Some(secret)).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        Arc::new(RwLock::new("leader".to_string())),
        "18.6".to_string(),
        Arc::new(vec![peer2_url]),
        Some(secret.to_string()),
    );
    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(Some(secret))),
        Arc::new(AuditLog::new("test-cluster", None, 100)),
    );

    // Pre-seed node activity via MetricsRing and verify 60-sample capacity cap.
    api_state
        .metrics_ring
        .observe_node(
            1,
            Some(PgNodeActivity {
                tup_returned: 70,
                tup_fetched: 30,
                tup_inserted: 5,
                ..Default::default()
            }),
        )
        .await;

    for i in 0..65u64 {
        let snap = ClusterMetricsSnapshot {
            timestamp_ms: 1_000 + i * 5_000,
            nodes: Vec::new(),
            activity: Default::default(),
            backups: Default::default(),
        };
        api_state.metrics_ring.push(snap).await;
    }

    let app = build_api_router(api_state.clone());
    let (token, _) = api_state.token_signer.sign("postgres").unwrap();

    let snap_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/metrics/snapshot")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(snap_resp.status(), StatusCode::OK);
    let snap_bytes = axum::body::to_bytes(snap_resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let snap: ClusterMetricsSnapshot = serde_json::from_slice(&snap_bytes).unwrap();
    assert_eq!(snap.nodes.len(), 2);
    let n1 = snap.nodes.iter().find(|n| n.node_id == 1).unwrap();
    assert_eq!(n1.pg_reads, 100);
    assert_eq!(n1.pg_writes, 5);
    assert_eq!(snap.activity.reads_total, 100);
    assert_eq!(snap.activity.writes_total, 5);

    let hist_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/metrics/history")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hist_resp.status(), StatusCode::OK);
    let hist_bytes = axum::body::to_bytes(hist_resp.into_body(), 512 * 1024)
        .await
        .unwrap();
    let hist: Vec<ClusterMetricsSnapshot> = serde_json::from_slice(&hist_bytes).unwrap();
    assert_eq!(hist.len(), 60);
    assert_eq!(hist.first().unwrap().timestamp_ms, 1_000 + 5 * 5_000);

    // Direct build_metrics_snapshot works cleanly too
    let direct = build_metrics_snapshot(&api_state).await;
    assert_eq!(direct.nodes.len(), 2);

    peer2_task.abort();
}

#[tokio::test]
async fn test_cluster_status_nodes_logs_config_and_audit_logs() {
    let secret = "cluster-secret";
    let (peer2_url, _peer2_role, peer2_task) =
        spawn_mock_control_peer(2, "standby", 18082, Some(secret)).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        Arc::new(RwLock::new("leader".to_string())),
        "18.6".to_string(),
        Arc::new(vec![peer2_url]),
        Some(secret.to_string()),
    );
    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    audit_log
        .append(
            AuditEventKind::NodeUp,
            Some(1),
            Some("127.0.0.1:5432"),
            "Node 1 came online",
            None,
        )
        .await;

    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(Some(secret))),
        audit_log,
    )
    .with_cluster_id("prod-pg");
    let app = build_api_router(api_state.clone());
    let (token, _) = api_state.token_signer.sign("postgres").unwrap();

    // /api/status
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let overview: ClusterOverview =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 128 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(overview.cluster_id, "prod-pg");
    assert_eq!(overview.total_nodes, 2);

    // /api/nodes
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/nodes")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let nodes: Vec<NodeSummary> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 128 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(nodes.len(), 2);

    // /api/nodes/1/logs (local) and /api/nodes/2/logs (forwarded to peer control API)
    for node_id in [1u64, 2u64] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/nodes/{node_id}/logs?limit=25"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let logs: NodeLogsResponse = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 128 * 1024).await.unwrap(),
        )
        .unwrap();
        assert_eq!(logs.node_id, node_id);
    }

    // /api/nodes/2/config/postgresql.conf (forwarded to peer)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/nodes/2/config/postgresql.conf")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cfg: NodeConfigResponse =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 128 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(cfg.node_id, 2);

    // /api/audit-logs
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/audit-logs?kind=node_up")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let audits: AuditListResponse =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 128 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(audits.total, 1);
    assert_eq!(audits.events[0].detail, "Node 1 came online");

    peer2_task.abort();
}

#[tokio::test]
async fn test_sql_explorer_and_security_and_standby_write() {
    let secret = "cluster-secret";
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

    let runner = Arc::new(RecordingSqlRunner::default());
    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres,bob"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(Some(secret))),
        Arc::new(AuditLog::new("test-cluster", None, 100)),
    )
    .with_sql_runner(runner.clone());

    let app = build_api_router(api_state.clone());
    // Authenticate as "bob" so we verify queries run as the logged-in Postgres user.
    let (bob_token, _) = api_state.token_signer.sign("bob").unwrap();

    // 1. /api/databases
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/databases")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let dbs: Vec<String> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(dbs, vec!["postgres", "app_db"]);

    // 2. /api/tables?db=app_db
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/tables?db=app_db")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tables: Vec<TableSummary> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(tables[0].name, "pgvisor_demo");

    // 3. /api/tables/pgvisor_demo/schema & data
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/tables/pgvisor_demo/schema?db=app_db")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cols: Vec<ColumnInfo> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert!(cols[0].is_primary_key);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/tables/pgvisor_demo/data?db=app_db")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let data: TableDataResponse =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(data.total_rows, 2);

    // 4. Read-only /api/sql succeeds on Standby
    let read_req = serde_json::to_vec(&SqlQueryRequest {
        query: "SELECT * FROM pgvisor_demo".into(),
        max_rows: None,
        database: Some("app_db".into()),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sql")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(read_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. Write /api/sql on Standby returns 409 NotLeader with leader_url
    let write_req = serde_json::to_vec(&SqlQueryRequest {
        query: "INSERT INTO pgvisor_demo(name) VALUES ('gamma')".into(),
        max_rows: None,
        database: Some("app_db".into()),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sql")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(write_req.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let err_json: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(err_json["leader_url"], "http://127.0.0.1:18082");

    // 6. Write /api/sql on Leader succeeds and executes as "bob"
    *local_role.write().await = "leader".to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sql")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(write_req.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let recorded = runner.calls.lock().await;
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|(u, _, _)| u == "bob"));

    // 7. Read-only SqlSecurityGuard forbids mutations with 403 MUTATION_FORBIDDEN
    let ro_state = api_state.with_security_guard(Arc::new(SqlSecurityGuard::new_read_only(
        std::time::Duration::from_secs(5),
        100,
    )));
    let ro_app = build_api_router(ro_state);
    let resp = ro_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sql")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(write_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let sql_err: SqlQueryError =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(sql_err.code, "MUTATION_FORBIDDEN");

    peer2_task.abort();
}

#[tokio::test]
async fn test_user_reads_api() {
    let runner = Arc::new(RecordingSqlRunner::default());
    let api_state = make_test_api_state(Some("postgres"), Some("cluster-secret"))
        .with_sql_runner(runner);
    let app = build_api_router(api_state.clone());
    let (token, _) = api_state.token_signer.sign("postgres").unwrap();

    // GET /api/users
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/users")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let roles: Vec<PgRole> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].rolname, "app_user");
    assert_eq!(roles[0].member_of, vec!["readers"]);

    // GET /api/users/app_user
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/users/app_user")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET /api/users/nonexistent -> 404
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/users/nonexistent")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // GET /api/users/app_user/privileges
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/users/app_user/privileges")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let privs: Vec<TablePrivilege> =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(privs.len(), 1);
    assert!(privs[0].select && privs[0].insert && !privs[0].delete);
}

#[tokio::test]
async fn test_cluster_write_routes_enforce_superuser_and_leader() {
    let secret = "cluster-secret";
    let (peer2_url, _peer2_role, peer2_task) =
        spawn_mock_control_peer(2, "leader", 18082, Some(secret)).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        Arc::new(RwLock::new("standby".to_string())),
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
    let app = build_api_router(api_state.clone());

    let (alice_token, _) = api_state.token_signer.sign("alice").unwrap();
    let (pg_token, _) = api_state.token_signer.sign("postgres").unwrap();

    // 1. Non-superuser gets 403 on write routes
    let switchover_body = serde_json::to_vec(&SwitchoverRequest { target_node_id: 2 }).unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cluster/switchover")
                .header("authorization", format!("Bearer {alice_token}"))
                .header("content-type", "application/json")
                .body(Body::from(switchover_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 2. Superuser on a standby gets 409 NotLeader with leader_url
    for (method, uri, body) in [
        ("POST", "/api/cluster/switchover", switchover_body),
        ("POST", "/api/nodes/2/start", Vec::new()),
        (
            "POST",
            "/api/users",
            serde_json::to_vec(&CreateRoleRequest {
                name: "bob".into(),
                password: Some("secret".into()),
                login: true,
                createdb: false,
                createrole: false,
                replication: false,
                connection_limit: -1,
            })
            .unwrap(),
        ),
        (
            "PUT",
            "/api/tables/pgvisor_demo/rows",
            serde_json::to_vec(&UpdateRowRequest {
                primary_keys: HashMap::from([("id".into(), serde_json::json!(1))]),
                values: HashMap::from([("name".into(), serde_json::json!("updated"))]),
            })
            .unwrap(),
        ),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("authorization", format!("Bearer {pg_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "expected 409 on standby for {method} {uri}"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["leader_url"], "http://127.0.0.1:18082");
    }

    peer2_task.abort();
}

async fn spawn_recording_control_peer(
    node_id: u64,
    initial_role: &str,
    api_port: u16,
) -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let role = Arc::new(RwLock::new(initial_role.to_string()));
    let c_promote = calls.clone();
    let r_promote = role.clone();
    let c_demote = calls.clone();
    let r_demote = role.clone();
    let c_repoint = calls.clone();
    let r_status = role.clone();

    let app = Router::new()
        .route(
            "/control/status",
            get(move || {
                let r = r_status.clone();
                async move {
                    let current = r.read().await.clone();
                    Json(crate::control::StatusResponse {
                        node_id,
                        role: current,
                        status: "running".into(),
                        child_pid: 1000 + node_id as u32,
                        pg_version: "18.6".into(),
                        uptime_secs: 100,
                        cpu_percent: 1.0,
                        memory_used_bytes: 0,
                        memory_total_bytes: 0,
                        disk_used_bytes: 0,
                        api_port,
                        activity: None,
                        restore_epoch: 0,
                    })
                }
            }),
        )
        .route(
            "/control/promote",
            post(move || {
                let c = c_promote.clone();
                let r = r_promote.clone();
                async move {
                    *r.write().await = "leader".into();
                    c.lock().await.push("promote".into());
                    Json(serde_json::json!({ "status": "ok" }))
                }
            }),
        )
        .route(
            "/control/demote",
            post(move || {
                let c = c_demote.clone();
                let r = r_demote.clone();
                async move {
                    *r.write().await = "fenced".into();
                    c.lock().await.push("demote".into());
                    Json(serde_json::json!({ "status": "ok" }))
                }
            }),
        )
        .route(
            "/control/repoint",
            post(move |Json(payload): Json<serde_json::Value>| {
                let c = c_repoint.clone();
                async move {
                    let conn = payload
                        .get("primary_conninfo")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    c.lock().await.push(format!("repoint:{conn}"));
                    Json(serde_json::json!({ "status": "ok" }))
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), calls, handle)
}

#[tokio::test]
async fn test_switchover_orchestration_against_mock_peers() {
    let (peer2_url, peer2_calls, peer2_task) =
        spawn_recording_control_peer(2, "standby", 18082).await;
    let (peer3_url, peer3_calls, peer3_task) =
        spawn_recording_control_peer(3, "standby", 18083).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(PostgresSupervisor::new(temp_dir.path()));
    let local_role = Arc::new(RwLock::new("leader".to_string()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        None,
        1,
        local_role.clone(),
        "18.6".to_string(),
        Arc::new(vec![peer2_url, peer3_url]),
        None,
    )
    .with_api_port(18081);

    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    let api_state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres,bob"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(None)),
        audit_log.clone(),
    );

    let app = build_api_router(api_state.clone());
    let (bob_token, _) = api_state.token_signer.sign("bob").unwrap();

    let body = serde_json::to_vec(&SwitchoverRequest { target_node_id: 2 }).unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cluster/switchover")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sw: SwitchoverResponse =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(sw.previous_leader_id, Some(1));
    assert_eq!(sw.new_leader_id, 2);

    // Local node 1 was demoted; peer 2 was promoted; peer 3 was repointed to 127.0.0.1
    assert_eq!(*local_role.read().await, "fenced");
    assert_eq!(&*peer2_calls.lock().await, &vec!["promote".to_string()]);
    let p3 = peer3_calls.lock().await;
    assert_eq!(p3.len(), 1);
    assert!(p3[0].starts_with("repoint:host=127.0.0.1 port=5432"));

    // Audit log recorded actor "bob"
    let (events, total) = audit_log.list(10, 0, Some(AuditEventKind::ElectionResult), None).await;
    assert_eq!(total, 1);
    assert!(events[0].detail.contains("bob"));

    peer2_task.abort();
    peer3_task.abort();
}

#[tokio::test]
async fn test_user_writes_and_row_mutations_audit_actor() {
    let runner = Arc::new(RecordingSqlRunner::default());
    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    let api_state = make_test_api_state(Some("postgres,bob"), Some("cluster-secret"))
        .with_sql_runner(runner.clone());
    let api_state = ApiState {
        audit_log: audit_log.clone(),
        ..api_state
    };
    let app = build_api_router(api_state.clone());
    let (bob_token, _) = api_state.token_signer.sign("bob").unwrap();

    // 1. Create role
    let create_req = serde_json::to_vec(&CreateRoleRequest {
        name: "app_writer".into(),
        password: Some("s3cr3t".into()),
        login: true,
        createdb: false,
        createrole: false,
        replication: false,
        connection_limit: 15,
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/users")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(create_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Alter role
    let alter_req = serde_json::to_vec(&AlterRoleRequest {
        login: Some(false),
        createdb: Some(true),
        createrole: None,
        replication: None,
        connection_limit: Some(5),
        password: None,
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/users/app_writer")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(alter_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Grant & revoke membership
    let mem_req = serde_json::to_vec(&RoleMembershipRequest {
        member_role: "app_writer".into(),
        group_role: "readers".into(),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/users/app_writer/memberships")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(mem_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/users/app_writer/memberships/readers")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Table privilege
    let priv_req = serde_json::to_vec(&TablePrivilegeRequest {
        table_name: "pgvisor_demo".into(),
        schema: Some("public".into()),
        privilege: TablePrivilegeKind::Insert,
        grant: true,
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/users/app_writer/privileges")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(priv_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. Update & delete row
    let update_req = serde_json::to_vec(&UpdateRowRequest {
        primary_keys: HashMap::from([("id".into(), serde_json::json!(1))]),
        values: HashMap::from([("name".into(), serde_json::json!("beta"))]),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/tables/pgvisor_demo/rows")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(update_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut_resp: RowMutationResponse =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(mut_resp.status, "ok");

    let del_req = serde_json::to_vec(&DeleteRowRequest {
        primary_keys: HashMap::from([("id".into(), serde_json::json!(1))]),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/tables/pgvisor_demo/rows")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(del_req))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 6. Drop role
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/users/app_writer")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify all queries ran as "bob" and audit log recorded every action with actor "bob"
    let calls = runner.calls.lock().await;
    assert!(calls.iter().any(|(u, _, sql)| u == "bob" && sql.contains("CREATE ROLE \"app_writer\"")));
    assert!(calls.iter().any(|(u, _, sql)| u == "bob" && sql.contains("UPDATE \"pgvisor_demo\"")));
    assert!(calls.iter().any(|(u, _, sql)| u == "bob" && sql.contains("DELETE FROM \"pgvisor_demo\"")));
    assert!(calls.iter().any(|(u, _, sql)| u == "bob" && sql.contains("DROP ROLE \"app_writer\"")));

    let (user_audits, _) = audit_log
        .list(20, 0, Some(AuditEventKind::UserPermission), None)
        .await;
    assert_eq!(user_audits.len(), 6);
    assert!(user_audits.iter().all(|e| e.detail.contains("bob")));

    let (row_audits, _) = audit_log
        .list(20, 0, Some(AuditEventKind::DangerousSql), None)
        .await;
    assert_eq!(row_audits.len(), 2);
    assert!(row_audits.iter().all(|e| e.detail.contains("bob")));
}

fn build_fs_backup_manager(dir: &std::path::Path) -> Arc<pgvisor_core::backup::BackupManager> {
    let builder = opendal::services::Fs::default().root(&dir.to_string_lossy());
    let op = opendal::Operator::new(builder).unwrap().finish();
    Arc::new(pgvisor_core::backup::BackupManager::new("test-cluster", op))
}

struct StubLeaderRestoreExecutor {
    fail: bool,
    recorded_snapshots: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::api::LeaderRestoreExecutor for StubLeaderRestoreExecutor {
    async fn execute_restore(
        &self,
        sidecar: &crate::control::SidecarState,
        payload: crate::control::RestorePayload,
    ) -> Result<(), crate::api::ApiError> {
        self.recorded_snapshots.lock().await.push(payload.snapshot_id);
        if self.fail {
            return Err(crate::api::ApiError::Internal(
                "simulated leader restore failure".into(),
            ));
        }
        sidecar.bump_restore_epoch();
        Ok(())
    }

    fn resync_retry_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(0)
    }
}

async fn spawn_restore_mock_peer(
    node_id: u64,
    role: &'static str,
) -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let c_prep = Arc::clone(&calls);
    let c_cancel = Arc::clone(&calls);
    let c_resync = Arc::clone(&calls);
    let app = axum::Router::new()
        .route(
            "/control/status",
            axum::routing::get(move || async move {
                Json(crate::control::StatusResponse {
                    node_id,
                    role: role.into(),
                    status: "running".into(),
                    child_pid: 2000 + node_id as u32,
                    pg_version: "18.6".into(),
                    uptime_secs: 60,
                    cpu_percent: 1.0,
                    memory_used_bytes: 0,
                    memory_total_bytes: 0,
                    disk_used_bytes: 0,
                    api_port: 18080 + node_id as u16,
                    activity: None,
                    restore_epoch: 0,
                })
            }),
        )
        .route(
            "/control/prepare-restore",
            axum::routing::post(move || {
                let c = Arc::clone(&c_prep);
                async move {
                    c.lock().await.push("prepare-restore".into());
                    Json(serde_json::json!({"status": "ok"}))
                }
            }),
        )
        .route(
            "/control/cancel-restore",
            axum::routing::post(move || {
                let c = Arc::clone(&c_cancel);
                async move {
                    c.lock().await.push("cancel-restore".into());
                    Json(serde_json::json!({"status": "ok"}))
                }
            }),
        )
        .route(
            "/control/resync",
            axum::routing::post(move |payload: Option<Json<serde_json::Value>>| {
                let c = Arc::clone(&c_resync);
                async move {
                    let conninfo = payload
                        .and_then(|Json(v)| v.get("primary_conninfo").and_then(|x| x.as_str()).map(String::from))
                        .unwrap_or_default();
                    c.lock().await.push(format!("resync:{conninfo}"));
                    Json(serde_json::json!({"status": "ok"}))
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), calls, handle)
}

#[tokio::test]
async fn test_backup_routes_enforce_auth_and_leader() {
    let data_dir = tempfile::tempdir().unwrap();
    let s3_dir = tempfile::tempdir().unwrap();
    let bm = build_fs_backup_manager(s3_dir.path());
    let supervisor = Arc::new(PostgresSupervisor::new(data_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        Some(bm),
        2,
        Arc::new(RwLock::new("standby".into())),
        "18.6".into(),
        Arc::new(vec![]),
        None,
    );
    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    let state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres,bob"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(None)),
        audit_log,
    );
    let app = build_api_router(state.clone());

    let (alice_token, _) = state.token_signer.sign("alice").unwrap();
    let (bob_token, _) = state.token_signer.sign("bob").unwrap();

    // Non-superuser gets 403 even on read routes
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/backups")
                .header("authorization", format!("Bearer {alice_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Superuser GET passes on standby; mutating backup routes return 409 NotLeader on standby
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/backups")
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    for (method, uri, body) in [
        ("POST", "/api/backups", b"{}".to_vec()),
        ("DELETE", "/api/backups/snap-1", vec![]),
        ("POST", "/api/backups/snap-1/restore", b"{}".to_vec()),
        (
            "POST",
            "/api/backups/quick-restore",
            b"{\"recovery_target_time\":\"2026-09-14 03:00:00\"}".to_vec(),
        ),
    ] {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {bob_token}"));
        if !body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        let resp = app
            .clone()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "{method} {uri} should return 409 on standby"
        );
    }
}

#[tokio::test]
async fn test_backup_crud_upload_pitr_window_and_lock_conflict() {
    let data_dir = tempfile::tempdir().unwrap();
    let s3_dir = tempfile::tempdir().unwrap();
    let bm = build_fs_backup_manager(s3_dir.path());
    let supervisor = Arc::new(PostgresSupervisor::new(data_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        Some(Arc::clone(&bm)),
        1,
        Arc::new(RwLock::new("leader".into())),
        "18.6".into(),
        Arc::new(vec![]),
        None,
    );
    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    let state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres,bob"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(None)),
        audit_log.clone(),
    );
    let app = build_api_router(state.clone());
    let (bob_token, _) = state.token_signer.sign("bob").unwrap();

    // 1. Create backup on leader (falls back to simulated archive when pg_basebackup unavailable)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/backups")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backup_type":"full","label":"phase7-test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: pgvisor_core::backup::BasebackupMeta =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(created.label.as_deref(), Some("phase7-test"));

    // 2. List, best, pitr-window, download
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/backups/{}/pitr-window", created.snapshot_id))
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let target_ok = (chrono::Utc::now() + chrono::Duration::seconds(2))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/backups/best?target_time={target_ok}"))
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/backups/{}/download", created.snapshot_id))
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let archive_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(!archive_bytes.is_empty());

    // 3. Active backup lock triggers 409 Conflict
    let lock = crate::control::acquire_backup_lock_local(&state.sidecar)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/backups/{}", created.snapshot_id))
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    crate::control::release_backup_lock_local(&state.sidecar, &lock.lock_token).await;

    // 4. Delete succeeds once lock is released
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/backups/{}", created.snapshot_id))
                .header("authorization", format!("Bearer {bob_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(bm.list_basebackups().await.unwrap().is_empty());
}

#[tokio::test]
async fn test_restore_orchestration_against_mock_peers_and_rollback() {
    let (peer2_url, peer2_calls, peer2_task) = spawn_restore_mock_peer(2, "standby").await;
    let (peer3_url, peer3_calls, peer3_task) = spawn_restore_mock_peer(3, "standby").await;

    let data_dir = tempfile::tempdir().unwrap();
    let s3_dir = tempfile::tempdir().unwrap();
    let bm = build_fs_backup_manager(s3_dir.path());
    let mut snap_meta = pgvisor_core::backup::BasebackupMeta::new(
        "snap-restore-test",
        chrono::Utc::now() - chrono::Duration::minutes(10),
        pgvisor_core::backup::BackupType::Full,
        "000000010000000000000001",
        0,
    );
    let tar = pgvisor_core::backup::create_simulated_basebackup(&mut snap_meta).unwrap();
    snap_meta.total_bytes = tar.len() as u64;
    bm.save_basebackup(&snap_meta, tar).await.unwrap();

    let supervisor = Arc::new(PostgresSupervisor::new(data_dir.path()));
    let sidecar = SidecarState::new(
        supervisor,
        Arc::new(RwLock::new(PostgresConfig::default())),
        Some(bm),
        1,
        Arc::new(RwLock::new("leader".into())),
        "18.6".into(),
        Arc::new(vec![peer2_url, peer3_url]),
        None,
    );
    let stub_ok = Arc::new(StubLeaderRestoreExecutor {
        fail: false,
        recorded_snapshots: Mutex::new(Vec::new()),
    });
    let audit_log = Arc::new(AuditLog::new("test-cluster", None, 100));
    let state = ApiState::new(
        sidecar,
        "127.0.0.1:5432",
        "postgres",
        Arc::new(SuperuserAllowlist::parse(Some("postgres,bob"), "postgres")),
        Arc::new(SessionTokenSigner::from_cluster_secret(None)),
        audit_log.clone(),
    )
    .with_leader_restore_executor(stub_ok.clone());

    let app = build_api_router(state.clone());
    let (bob_token, _) = state.token_signer.sign("bob").unwrap();

    // 1. Successful restore prepares standbys, executes leader restore, bumps restore_epoch, and resyncs standbys
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/backups/snap-restore-test/restore")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.sidecar.restore_epoch(), 1);
    assert_eq!(
        &*stub_ok.recorded_snapshots.lock().await,
        &vec!["snap-restore-test".to_string()]
    );

    for calls in [&peer2_calls, &peer3_calls] {
        let log = calls.lock().await;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], "prepare-restore");
        assert!(log[1].starts_with("resync:host=127.0.0.1 port=5432 user=postgres"));
    }

    let (audits, _) = audit_log
        .list(10, 0, Some(AuditEventKind::BackupRestored), None)
        .await;
    assert_eq!(audits.len(), 1);
    assert!(audits[0].detail.contains("bob"));

    // 2. Failing leader restore cancels Restoring state on standbys and releases the lock
    peer2_calls.lock().await.clear();
    peer3_calls.lock().await.clear();
    let stub_fail = Arc::new(StubLeaderRestoreExecutor {
        fail: true,
        recorded_snapshots: Mutex::new(Vec::new()),
    });
    let fail_state = state.with_leader_restore_executor(stub_fail);
    let fail_app = build_api_router(fail_state.clone());
    let resp = fail_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/backups/snap-restore-test/restore")
                .header("authorization", format!("Bearer {bob_token}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    for calls in [&peer2_calls, &peer3_calls] {
        let log = calls.lock().await;
        assert_eq!(*log, vec!["prepare-restore", "cancel-restore"]);
    }
    assert!(fail_state.sidecar.backup_lock.read().await.is_none());

    peer2_task.abort();
    peer3_task.abort();
}
