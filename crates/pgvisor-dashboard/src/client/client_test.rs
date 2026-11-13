use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use pgvisor_core::api::{
    AuditListResponse, ClusterMember, ClusterMembersResponse, ClusterOverview, LoginRequest,
    LoginResponse, MemberHealth, NodeHealthState, NodeRole, NodeSummary, PgRole, SidecarRole,
    TableDataResponse, TablePrivilege, TableSummary,
};
use pgvisor_core::backup::{BackupType, BasebackupMeta};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::client::{
    create_client_router, parse_sidecar_urls, ClientDashboardState, SidecarClient,
    SidecarClientError,
};

async fn spawn_mock_router(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), handle)
}

fn sample_overview() -> ClusterOverview {
    ClusterOverview {
        cluster_id: "test-cluster".into(),
        current_term: 1,
        leader_id: Some(1),
        leader_address: Some("pgvisor-node1:5432".into()),
        quorum_size: 2,
        total_nodes: 2,
        healthy_nodes: 2,
        last_backup_at: Some(Utc::now()),
        total_backups: 1,
        nodes: vec![NodeSummary {
            node_id: 1,
            address: "pgvisor-node1:5432".into(),
            role: NodeRole::Leader,
            state: NodeHealthState::Healthy,
            pg_version: "18.6".into(),
            replication_lag_bytes: 0,
            uptime_secs: 100,
            is_local: true,
            cpu_percent: 2.0,
            memory_used_bytes: 1000,
            memory_total_bytes: 2000,
            disk_used_bytes: 5000,
        }],
    }
}

#[tokio::test]
async fn test_sidecar_client_leader_discovery_and_409_redirect() {
    let parsed = parse_sidecar_urls(" http://n1:8081/ , http://n2:8081 , http://n1:8081 ");
    assert_eq!(parsed, vec!["http://n1:8081", "http://n2:8081"]);

    let leader_hits = Arc::new(AtomicUsize::new(0));
    let leader_hits_route = Arc::clone(&leader_hits);
    let leader_app = Router::new()
        .route("/api/status", get(|| async { Json(sample_overview()) }))
        .route(
            "/api/cluster/switchover",
            post(move || {
                let hits = Arc::clone(&leader_hits_route);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"status": "ok", "new_leader_id": 2}))
                }
            }),
        );
    let (leader_url, leader_task) = spawn_mock_router(leader_app).await;

    let leader_url_for_members = leader_url.clone();
    let leader_url_for_redirect = leader_url.clone();
    let standby_app = Router::new()
        .route(
            "/api/cluster/members",
            get(move || {
                let url = leader_url_for_members.clone();
                async move {
                    Json(ClusterMembersResponse {
                        leader_url: Some(url.clone()),
                        members: vec![ClusterMember {
                            node_id: Some(2),
                            api_url: Some(url),
                            role: Some(SidecarRole::Leader),
                            health: MemberHealth::Healthy,
                            is_self: false,
                        }],
                    })
                }
            }),
        )
        .route(
            "/api/cluster/switchover",
            post(move || {
                let url = leader_url_for_redirect.clone();
                async move {
                    (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "status": "not_leader",
                            "error": "Not cluster leader",
                            "leader_url": url,
                        })),
                    )
                }
            }),
        );
    let (standby_url, standby_task) = spawn_mock_router(standby_app).await;

    // Include an unreachable port first to test connection failover, then standby
    let dead_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dead_url = format!("http://127.0.0.1:{dead_port}");
    let client = SidecarClient::new(vec![dead_url, standby_url.clone()]).unwrap();

    // 1. Discover leader skips dead_url and caches leader_url from standby /api/cluster/members
    let discovered = client.discover_leader("tok").await.unwrap();
    assert_eq!(discovered.as_deref(), Some(leader_url.as_str()));
    assert_eq!(client.cached_leader().await.as_deref(), Some(leader_url.as_str()));

    // 2. Write fails over even if cached leader points stale to standby -> 409 NotLeader redirects to leader_url
    let stale_client = SidecarClient::new(vec![standby_url.clone()]).unwrap();
    let resp = stale_client
        .send_raw(
            true,
            reqwest::Method::POST,
            "/api/cluster/switchover",
            Some("tok"),
            Some("application/json"),
            Bytes::from_static(br#"{"target_node_id":2}"#),
        )
        .await
        .unwrap();
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(leader_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        stale_client.cached_leader().await.as_deref(),
        Some(leader_url.as_str())
    );

    leader_task.abort();
    standby_task.abort();
}

#[tokio::test]
async fn test_client_router_login_cookie_and_403_superuser_message() {
    let sidecar_app = Router::new()
        .route(
            "/api/auth/login",
            post(|Json(req): Json<LoginRequest>| async move {
                match req.username.as_str() {
                    "bob" if req.password == "secret" => Ok(Json(LoginResponse {
                        token: "jwt-bob-token".into(),
                        username: "bob".into(),
                        expires_at: 4_000_000_000,
                    })),
                    "alice" => Err((
                        StatusCode::FORBIDDEN,
                        Json(serde_json::json!({
                            "status": "forbidden",
                            "error": "user is not a PgVisor superuser"
                        })),
                    )),
                    _ => Err((
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "status": "unauthorized",
                            "error": "Invalid credentials"
                        })),
                    )),
                }
            }),
        )
        .route("/api/status", get(|| async { Json(sample_overview()) }))
        .route(
            "/api/backups",
            get(|| async { Json(Vec::<BasebackupMeta>::new()) }),
        );
    let (sidecar_url, sidecar_task) = spawn_mock_router(sidecar_app).await;

    let client = Arc::new(SidecarClient::new(vec![sidecar_url]).unwrap());
    let state = Arc::new(ClientDashboardState::new(client, "test-cluster"));
    let app = create_client_router(state);

    // 1. Unauthenticated / redirects to /login
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers().get("location").unwrap(), "/login");

    // 2. GET /login renders username + password inputs
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let login_html = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(login_html.contains("name=\"username\""));
    assert!(login_html.contains("name=\"password\""));

    // 3. Non-superuser gets 403 and exact "user is not a PgVisor superuser" message
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=alice&password=any"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let err_html = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(err_html.contains("user is not a PgVisor superuser"));

    // 4. Invalid credentials return 401
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=bob&password=wrong"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 5. Valid superuser login sets HttpOnly cookie and redirects to /
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=bob&password=secret"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers().get("location").unwrap(), "/");
    let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("pgvisor_token=jwt-bob-token"));
    assert!(cookie.contains("HttpOnly"));

    // 6. Authenticated GET / renders overview
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("cookie", "pgvisor_token=jwt-bob-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("test-cluster"));

    sidecar_task.abort();
}

#[tokio::test]
async fn test_client_router_ssr_pages_reverse_proxy_and_401_403_redirect() {
    let sidecar_app = Router::new()
        .route(
            "/api/auth/login",
            post(|| async {
                Json(serde_json::json!({
                    "token": "api-login-tok",
                    "username": "postgres",
                    "expires_at": "2099-01-01T00:00:00Z"
                }))
            }),
        )
        .route(
            "/api/nodes",
            get(|headers: axum::http::HeaderMap| async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                if auth == "Bearer expired" {
                    return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "expired"}))).into_response();
                }
                if auth == "Bearer alice" {
                    return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "forbidden"}))).into_response();
                }
                Json(sample_overview().nodes).into_response()
            }),
        )
        .route("/api/databases", get(|| async { Json(vec!["postgres"]) }))
        .route(
            "/api/tables",
            get(|| async {
                Json(vec![TableSummary {
                    name: "orders".into(),
                    schema: "public".into(),
                    estimated_rows: 1,
                    size_pretty: "8 kB".into(),
                }])
            }),
        )
        .route(
            "/api/tables/orders/data",
            get(|| async {
                Json(TableDataResponse {
                    table_name: "orders".into(),
                    columns: vec!["id".into()],
                    primary_keys: vec!["id".into()],
                    rows: vec![vec![Some("1".into())]],
                    total_rows: 1,
                    limit: 50,
                    offset: 0,
                })
            }),
        )
        .route(
            "/api/backups",
            get(|| async {
                Json(vec![BasebackupMeta::new(
                    "snap-test",
                    Utc::now(),
                    BackupType::Full,
                    "000000010000000000000001",
                    1024,
                )])
            }),
        )
        .route(
            "/api/users",
            get(|| async {
                Json(vec![PgRole {
                    rolname: "app".into(),
                    rolcanlogin: true,
                    rolcreatedb: false,
                    rolcreaterole: false,
                    rolreplication: false,
                    rolsuper: false,
                    rolconnlimit: -1,
                    member_of: vec![],
                }])
            }),
        )
        .route(
            "/api/users/app/privileges",
            get(|| async { Json(Vec::<TablePrivilege>::new()) }),
        )
        .route(
            "/api/audit-logs",
            get(|| async {
                Json(AuditListResponse {
                    events: vec![],
                    total: 0,
                    limit: 50,
                    offset: 0,
                    dangerous_sql_count: 0,
                    latest_pitr_target: None,
                })
            }),
        )
        .route(
            "/api/cluster/members",
            get(|| async {
                Json(ClusterMembersResponse {
                    leader_url: None,
                    members: vec![],
                })
            }),
        )
        .route(
            "/api/backups/snap-test",
            axum::routing::delete(|| async {
                // Non-leader 409 conflict (backup lock already held) must pass through without NotLeader retry
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "status": "conflict",
                        "error": "A backup or restore operation is already in progress."
                    })),
                )
            }),
        );
    use axum::response::IntoResponse;

    let (sidecar_url, sidecar_task) = spawn_mock_router(sidecar_app).await;
    let client = Arc::new(SidecarClient::new(vec![sidecar_url]).unwrap());
    let state = Arc::new(ClientDashboardState::new(client, "test-cluster"));
    let app = create_client_router(state);

    // 1. All SSR pages render cleanly with valid cookie
    for uri in ["/nodes", "/nodes/1/inspect", "/tables", "/sql", "/backups", "/users", "/audit-logs"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", "pgvisor_token=valid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "SSR page {uri} failed");
    }

    // 2. Any page receiving 401 or 403 from sidecar redirects to /login and clears cookie
    for bad_token in ["expired", "alice"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/nodes")
                    .header("cookie", format!("pgvisor_token={bad_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers().get("location").unwrap(), "/login");
        assert!(resp
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
    }

    // 3. Reverse proxy allows unauthenticated POST /api/auth/login and preserves 409 conflict
    let login_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"postgres","password":"postgres"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/backups/snap-test")
                .header("cookie", "pgvisor_token=valid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let err_json: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 16 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(err_json["status"], "conflict");

    sidecar_task.abort();
    let _check_unused = SidecarClientError::EmptyUrls;
}
