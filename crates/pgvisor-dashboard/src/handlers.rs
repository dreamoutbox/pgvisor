use std::sync::Arc;
use std::time::Instant;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use chrono::Utc;
use pgvisor_core::backup::{BackupType, BasebackupMeta};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::models::{
    format_bytes, BackupItemView, BackupOverviewSummary, ClusterOverview, ColumnInfo,
    CreateBackupRequest, NodeHealthState, NodeRole, NodeSummary, RestoreBackupRequest,
    SqlQueryError, SqlQueryRequest, SqlQueryResult, TableDataResponse, TableSummary,
};
use crate::security::{SecurityError, SqlSecurityGuard};
use crate::templates::{
    BackupsTemplate, LoginTemplate, NodesTemplate, OverviewTemplate, TablesTemplate,
};

/// Abstraction for executing SQL queries on PostgreSQL backends.
#[async_trait::async_trait]
pub trait SqlExecutor: Send + Sync {
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<SqlQueryResult, String>;
}

/// In-memory or proxy-connected executor for the dashboard console.
pub struct StandaloneSqlExecutor;

#[async_trait::async_trait]
impl SqlExecutor for StandaloneSqlExecutor {
    async fn execute(&self, sql: &str, max_rows: usize) -> Result<SqlQueryResult, String> {
        let start = Instant::now();
        // Safe standard catalog query simulation for standalone dashboard mode
        let upper = sql.to_uppercase();
        let (columns, rows) = if upper.contains("INFORMATION_SCHEMA.TABLES") {
            (
                vec!["table_name".into(), "table_schema".into()],
                vec![vec!["pgvisor_demo".into(), "public".into()]],
            )
        } else if upper.contains("INFORMATION_SCHEMA.COLUMNS") {
            (
                vec![
                    "column_name".into(),
                    "data_type".into(),
                    "is_nullable".into(),
                    "column_default".into(),
                ],
                vec![
                    vec![
                        "id".into(),
                        "integer".into(),
                        "NO".into(),
                        "nextval(...)".into(),
                    ],
                    vec![
                        "name".into(),
                        "character varying".into(),
                        "NO".into(),
                        "NULL".into(),
                    ],
                    vec![
                        "status".into(),
                        "character varying".into(),
                        "YES".into(),
                        "'active'".into(),
                    ],
                    vec!["counter".into(), "integer".into(), "YES".into(), "0".into()],
                    vec![
                        "created_at".into(),
                        "timestamp with time zone".into(),
                        "YES".into(),
                        "now()".into(),
                    ],
                    vec![
                        "updated_at".into(),
                        "timestamp with time zone".into(),
                        "YES".into(),
                        "now()".into(),
                    ],
                ],
            )
        } else if upper.contains("COUNT(*)") {
            (vec!["count".into()], vec![vec!["4".into()]])
        } else if upper.contains("PGVISOR_DEMO") {
            (
                vec![
                    "id".into(),
                    "name".into(),
                    "status".into(),
                    "counter".into(),
                    "created_at".into(),
                    "updated_at".into(),
                ],
                vec![
                    vec![
                        "1".into(),
                        "alpha".into(),
                        "active".into(),
                        "10".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "2".into(),
                        "beta".into(),
                        "active".into(),
                        "20".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "3".into(),
                        "gamma".into(),
                        "active".into(),
                        "130".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                    vec![
                        "4".into(),
                        "delta".into(),
                        "archived".into(),
                        "40".into(),
                        "2026-09-04 21:58:21".into(),
                        "2026-09-04 21:58:21".into(),
                    ],
                ],
            )
        } else if upper.contains("PG_STAT_DATABASE") {
            (
                vec![
                    "datname".into(),
                    "numbackends".into(),
                    "xact_commit".into(),
                    "xact_rollback".into(),
                ],
                vec![
                    vec!["postgres".into(), "4".into(), "14285".into(), "12".into()],
                    vec![
                        "pgvisor_app".into(),
                        "18".into(),
                        "98420".into(),
                        "3".into(),
                    ],
                ],
            )
        } else if upper.contains("VERSION()") {
            (
                vec!["version".into()],
                vec![vec![
                    "PostgreSQL 16.3 on x86_64-pc-linux-gnu, compiled by gcc, 64-bit".into(),
                ]],
            )
        } else {
            (
                vec!["result".into()],
                vec![vec![format!("Executed successfully: {}", sql)]],
            )
        };

        let row_count = rows.len().min(max_rows);
        let elapsed = start.elapsed().as_millis() as u64;

        Ok(SqlQueryResult {
            columns,
            rows: rows.into_iter().take(row_count).collect(),
            execution_time_ms: elapsed,
            row_count,
            truncated: false,
        })
    }
}

/// Abstraction for backup and restore operations across physical storage and Postgres nodes.
#[async_trait::async_trait]
pub trait BackupService: Send + Sync {
    async fn list_backups(&self) -> Result<Vec<BasebackupMeta>, String>;
    async fn create_backup(
        &self,
        backup_type: BackupType,
        label: Option<String>,
    ) -> Result<BasebackupMeta, String>;
    async fn restore_backup(
        &self,
        snapshot_id: &str,
        target_time: Option<String>,
    ) -> Result<String, String>;
    async fn delete_backup(&self, snapshot_id: &str) -> Result<(), String>;
    async fn get_backup_archive(
        &self,
        snapshot_id: &str,
    ) -> Result<(BasebackupMeta, Vec<u8>), String>;
    fn storage_info(&self) -> (String, String, u32) {
        ("http://127.0.0.1:9000".into(), "pgvisor-backups".into(), 7)
    }
}

/// In-memory / mock backup service for standalone dashboard operation and testing.
pub struct StandaloneBackupService {
    backups: Arc<RwLock<Vec<BasebackupMeta>>>,
    endpoint: String,
    bucket: String,
}

impl StandaloneBackupService {
    pub fn new() -> Self {
        let initial = vec![
            BasebackupMeta {
                snapshot_id: "snap-20260904-200000".into(),
                created_at: Utc::now() - chrono::Duration::hours(5),
                backup_type: BackupType::Full,
                label: Some("pre-migration-snapshot".into()),
                start_wal: "000000010000000000000001".into(),
                stop_wal: Some("000000010000000000000002".into()),
                total_bytes: 14_850_000,
            },
            BasebackupMeta {
                snapshot_id: "snap-20260904-210000".into(),
                created_at: Utc::now() - chrono::Duration::hours(4),
                backup_type: BackupType::Incremental,
                label: None,
                start_wal: "000000010000000000000003".into(),
                stop_wal: Some("000000010000000000000004".into()),
                total_bytes: 2_450_000,
            },
        ];
        Self {
            backups: Arc::new(RwLock::new(initial)),
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "pgvisor-backups".into(),
        }
    }
}

impl Default for StandaloneBackupService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BackupService for StandaloneBackupService {
    async fn list_backups(&self) -> Result<Vec<BasebackupMeta>, String> {
        let mut list = self.backups.read().await.clone();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(list)
    }

    async fn create_backup(
        &self,
        backup_type: BackupType,
        label: Option<String>,
    ) -> Result<BasebackupMeta, String> {
        let now = Utc::now();
        let meta = BasebackupMeta {
            snapshot_id: format!("snap-{}", now.format("%Y%m%d-%H%M%S")),
            created_at: now,
            backup_type,
            label,
            start_wal: "000000010000000000000010".into(),
            stop_wal: Some("000000010000000000000011".into()),
            total_bytes: match backup_type {
                BackupType::Full => 15_200_000,
                BackupType::Incremental => 1_850_000,
            },
        };

        let mut lock = self.backups.write().await;
        lock.push(meta.clone());
        Ok(meta)
    }

    async fn restore_backup(
        &self,
        snapshot_id: &str,
        target_time: Option<String>,
    ) -> Result<String, String> {
        let lock = self.backups.read().await;
        if lock.iter().any(|b| b.snapshot_id == snapshot_id) {
            let detail = target_time
                .map(|t| format!(" (PITR target: {})", t))
                .unwrap_or_default();
            Ok(format!(
                "Snapshot {} successfully restored{}",
                snapshot_id, detail
            ))
        } else {
            Err(format!("Snapshot {} not found", snapshot_id))
        }
    }

    async fn delete_backup(&self, snapshot_id: &str) -> Result<(), String> {
        let mut lock = self.backups.write().await;
        let before_len = lock.len();
        lock.retain(|b| b.snapshot_id != snapshot_id);
        if lock.len() == before_len {
            Err(format!("Snapshot {} not found", snapshot_id))
        } else {
            Ok(())
        }
    }

    async fn get_backup_archive(
        &self,
        snapshot_id: &str,
    ) -> Result<(BasebackupMeta, Vec<u8>), String> {
        let lock = self.backups.read().await;
        let meta = lock
            .iter()
            .find(|b| b.snapshot_id == snapshot_id)
            .cloned()
            .ok_or_else(|| format!("Snapshot {} not found", snapshot_id))?;

        let dummy_tar_gz = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
        Ok((meta, dummy_tar_gz))
    }

    fn storage_info(&self) -> (String, String, u32) {
        (self.endpoint.clone(), self.bucket.clone(), 7)
    }
}

/// Shared application state for dashboard HTTP handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub overview: Arc<RwLock<ClusterOverview>>,
    pub security_guard: Arc<SqlSecurityGuard>,
    pub sql_executor: Arc<dyn SqlExecutor>,
    pub backup_service: Arc<dyn BackupService>,
    pub admin_token: Option<String>,
}

impl DashboardState {
    pub fn new(cluster_id: &str, admin_token: Option<String>) -> Self {
        let initial_overview = ClusterOverview {
            cluster_id: cluster_id.to_string(),
            current_term: 1,
            leader_id: Some(1),
            leader_address: Some("127.0.0.1:5432".to_string()),
            quorum_size: 2,
            total_nodes: 3,
            healthy_nodes: 3,
            last_backup_at: None,
            total_backups: 0,
            nodes: vec![
                NodeSummary {
                    node_id: 1,
                    address: "127.0.0.1:5432".into(),
                    role: NodeRole::Leader,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 0,
                    uptime_secs: 3600,
                    is_local: true,
                },
                NodeSummary {
                    node_id: 2,
                    address: "127.0.0.1:5433".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 64,
                    uptime_secs: 3580,
                    is_local: false,
                },
                NodeSummary {
                    node_id: 3,
                    address: "127.0.0.1:5434".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 128,
                    uptime_secs: 3550,
                    is_local: false,
                },
            ],
        };

        Self {
            overview: Arc::new(RwLock::new(initial_overview)),
            security_guard: Arc::new(SqlSecurityGuard::default()),
            sql_executor: Arc::new(StandaloneSqlExecutor),
            backup_service: Arc::new(StandaloneBackupService::new()),
            admin_token,
        }
    }
}

/// GET / -> Renders cluster overview dashboard
pub async fn get_overview(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    if let Ok(backups) = state.backup_service.list_backups().await {
        let mut overview_write = state.overview.write().await;
        overview_write.total_backups = backups.len();
        if let Some(latest) = backups.first() {
            overview_write.last_backup_at = Some(latest.created_at);
        }
    }

    let overview = state.overview.read().await;
    let template = OverviewTemplate {
        overview: &overview,
        auth_enabled: state.admin_token.is_some(),
    };
    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render overview template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /nodes -> Renders detailed node health inspection
pub async fn get_nodes(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let overview = state.overview.read().await;
    let template = NodesTemplate {
        nodes: &overview.nodes,
        auth_enabled: state.admin_token.is_some(),
    };
    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render nodes template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct TablesQuery {
    pub table: Option<String>,
    pub tab: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub sql: Option<String>,
}

/// GET /tables -> Renders database table browser and merged SQL explorer
pub async fn get_tables_page(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<TablesQuery>,
) -> Result<Html<String>, StatusCode> {
    let tables_sql = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";
    let tables_res = state
        .sql_executor
        .execute(tables_sql, 100)
        .await
        .unwrap_or_else(|_| SqlQueryResult {
            columns: vec!["table_name".into()],
            rows: Vec::new(),
            execution_time_ms: 0,
            row_count: 0,
            truncated: false,
        });

    let tables: Vec<TableSummary> = tables_res
        .rows
        .into_iter()
        .filter_map(|mut row| {
            let name = row.drain(..).next()?;
            Some(TableSummary {
                name,
                schema: "public".into(),
                estimated_rows: 0,
                size_pretty: "-".into(),
            })
        })
        .collect();

    let active_table_name = params.table.as_deref().or_else(|| {
        if params.tab.as_deref() == Some("sql") {
            None
        } else {
            tables.first().map(|t| t.name.as_str())
        }
    });

    let active_tab = params.tab.as_deref().unwrap_or("data");

    let mut columns = Vec::new();
    let mut data_columns = Vec::new();
    let mut data_rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut total_rows = 0u64;
    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    if let Some(tbl) = active_table_name {
        if tbl.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            if active_tab == "schema" {
                let schema_sql = format!(
                    "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '{}' ORDER BY ordinal_position;",
                    tbl
                );
                if let Ok(res) = state.sql_executor.execute(&schema_sql, 200).await {
                    for row in res.rows {
                        if row.len() >= 3 {
                            let name = row[0].clone();
                            let data_type = row[1].clone();
                            let is_nullable = row[2].eq_ignore_ascii_case("YES");
                            let default_val =
                                if row.len() > 3 && !row[3].is_empty() && row[3] != "NULL" {
                                    Some(row[3].clone())
                                } else {
                                    None
                                };
                            let is_primary_key = name.eq_ignore_ascii_case("id");
                            columns.push(ColumnInfo {
                                name,
                                data_type,
                                is_nullable,
                                default_value: default_val,
                                is_primary_key,
                            });
                        }
                    }
                }
            } else if active_tab == "data" {
                let count_sql = format!("SELECT count(*) FROM {};", tbl);
                if let Ok(count_res) = state.sql_executor.execute(&count_sql, 1).await {
                    if let Some(r) = count_res.rows.first() {
                        if let Some(c_str) = r.first() {
                            total_rows = c_str.parse().unwrap_or(0);
                        }
                    }
                }

                let data_sql = format!("SELECT * FROM {} LIMIT {} OFFSET {};", tbl, limit, offset);
                if let Ok(data_res) = state.sql_executor.execute(&data_sql, limit).await {
                    data_columns = data_res.columns;
                    data_rows = data_res
                        .rows
                        .into_iter()
                        .map(|row| row.into_iter().map(Some).collect())
                        .collect();
                }
            }
        }
    }

    let total_pages = if total_rows > 0 {
        ((total_rows as usize + limit - 1) / limit).max(1)
    } else if !data_rows.is_empty() {
        1
    } else {
        0
    };

    let template = TablesTemplate {
        tables: &tables,
        active_table: active_table_name,
        active_tab,
        columns: &columns,
        data_columns: &data_columns,
        data_rows: &data_rows,
        total_rows,
        page,
        limit,
        total_pages,
        initial_sql: params.sql.as_deref(),
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render tables template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /sql -> Renders interactive SQL console (delegates to unified explorer in SQL tab)
pub async fn get_sql_console(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    get_tables_page(
        State(state),
        Query(TablesQuery {
            tab: Some("sql".into()),
            ..Default::default()
        }),
    )
    .await
}

/// GET /api/tables -> Returns JSON list of public tables
pub async fn api_list_tables(State(state): State<Arc<DashboardState>>) -> Json<Vec<TableSummary>> {
    let sql = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";
    let res = state
        .sql_executor
        .execute(sql, 200)
        .await
        .unwrap_or_else(|_| SqlQueryResult {
            columns: vec!["table_name".into()],
            rows: Vec::new(),
            execution_time_ms: 0,
            row_count: 0,
            truncated: false,
        });

    let tables = res
        .rows
        .into_iter()
        .filter_map(|mut row| {
            let name = row.drain(..).next()?;
            Some(TableSummary {
                name,
                schema: "public".into(),
                estimated_rows: 0,
                size_pretty: "-".into(),
            })
        })
        .collect();

    Json(tables)
}

/// GET /api/tables/:table/schema -> Returns column metadata for a table
pub async fn api_table_schema(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
) -> Result<Json<Vec<ColumnInfo>>, (StatusCode, String)> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let sql = format!(
        "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '{}' ORDER BY ordinal_position;",
        table
    );
    let res = state
        .sql_executor
        .execute(&sql, 200)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let columns = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if row.len() >= 3 {
                let name = row[0].clone();
                let data_type = row[1].clone();
                let is_nullable = row[2].eq_ignore_ascii_case("YES");
                let default_val = if row.len() > 3 && !row[3].is_empty() && row[3] != "NULL" {
                    Some(row[3].clone())
                } else {
                    None
                };
                let is_primary_key = name.eq_ignore_ascii_case("id");
                Some(ColumnInfo {
                    name,
                    data_type,
                    is_nullable,
                    default_value: default_val,
                    is_primary_key,
                })
            } else {
                None
            }
        })
        .collect();

    Ok(Json(columns))
}

/// GET /api/tables/:table/data?limit=50&offset=0 -> Returns paginated rows
pub async fn api_table_data(
    State(state): State<Arc<DashboardState>>,
    Path(table): Path<String>,
    Query(params): Query<TablesQuery>,
) -> Result<Json<TableDataResponse>, (StatusCode, String)> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let page = params.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let count_sql = format!("SELECT count(*) FROM {};", table);
    let total_rows = state
        .sql_executor
        .execute(&count_sql, 1)
        .await
        .ok()
        .and_then(|r| r.rows.first()?.first()?.parse::<u64>().ok())
        .unwrap_or(0);

    let sql = format!("SELECT * FROM {} LIMIT {} OFFSET {};", table, limit, offset);
    let res = state
        .sql_executor
        .execute(&sql, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(TableDataResponse {
        table_name: table,
        columns: res.columns,
        rows: res
            .rows
            .into_iter()
            .map(|r| r.into_iter().map(Some).collect())
            .collect(),
        total_rows,
        limit,
        offset,
    }))
}

/// GET /api/status -> Returns cluster health status JSON
pub async fn api_status(State(state): State<Arc<DashboardState>>) -> Json<ClusterOverview> {
    let overview = state.overview.read().await.clone();
    Json(overview)
}

/// POST /api/sql -> Validates read-only safety, enforces statement timeout, executes SQL
pub async fn api_execute_sql(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<SqlQueryRequest>,
) -> Result<Json<SqlQueryResult>, (StatusCode, Json<SqlQueryError>)> {
    let max_rows = payload
        .max_rows
        .unwrap_or(state.security_guard.max_result_rows)
        .min(state.security_guard.max_result_rows);

    // 1. Validate SQL query according to security settings
    let safe_sql = match state.security_guard.validate_sql(&payload.query) {
        Ok(sql) => sql,
        Err(err) => {
            warn!(query = %payload.query, ?err, "SQL security validation rejected query");
            let (status, code) = match err {
                SecurityError::MutationForbidden(_) => {
                    (StatusCode::FORBIDDEN, "MUTATION_FORBIDDEN")
                }
                SecurityError::MultiStatementForbidden(_) => {
                    (StatusCode::BAD_REQUEST, "MULTI_STATEMENT_FORBIDDEN")
                }
                SecurityError::EmptyQuery => (StatusCode::BAD_REQUEST, "EMPTY_QUERY"),
                _ => (StatusCode::BAD_REQUEST, "SECURITY_ERROR"),
            };
            return Err((
                status,
                Json(SqlQueryError {
                    code: code.into(),
                    message: err.to_string(),
                }),
            ));
        }
    };

    info!(sql = %safe_sql, max_rows, "Executing guarded SQL console query");

    // 2. Execute query under statement timeout protection
    let timeout = state.security_guard.max_execution_timeout;
    let exec_res =
        tokio::time::timeout(timeout, state.sql_executor.execute(&safe_sql, max_rows)).await;

    match exec_res {
        Ok(Ok(result)) => Ok(Json(result)),
        Ok(Err(db_err)) => {
            error!(?db_err, "Database error executing console SQL");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SqlQueryError {
                    code: "EXECUTION_FAILED".into(),
                    message: db_err,
                }),
            ))
        }
        Err(_) => {
            warn!(?timeout, "Console SQL query timed out");
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(SqlQueryError {
                    code: "STATEMENT_TIMEOUT".into(),
                    message: format!(
                        "Query execution exceeded statement timeout of {:?}",
                        timeout
                    ),
                }),
            ))
        }
    }
}

/// GET /backups -> Renders backup management page
pub async fn get_backups_page(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let list = state
        .backup_service
        .list_backups()
        .await
        .unwrap_or_default();
    let (storage_endpoint, storage_bucket, retention_days) = state.backup_service.storage_info();

    let total_bytes: u64 = list.iter().map(|b| b.total_bytes).sum();
    let latest_backup = list
        .first()
        .map(|b| b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let summary = BackupOverviewSummary {
        total_backups: list.len(),
        latest_backup,
        total_size_pretty: format_bytes(total_bytes),
        retention_days,
        storage_endpoint,
        storage_bucket,
    };

    let backup_items: Vec<BackupItemView> = list
        .into_iter()
        .map(|b| {
            let backup_type = match b.backup_type {
                BackupType::Full => "full".to_string(),
                BackupType::Incremental => "incremental".to_string(),
            };
            BackupItemView {
                snapshot_id: b.snapshot_id,
                created_at: b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                backup_type,
                label: b.label,
                start_wal: b.start_wal,
                stop_wal: b.stop_wal.unwrap_or_else(|| "-".to_string()),
                size_pretty: format_bytes(b.total_bytes),
                total_bytes: b.total_bytes,
            }
        })
        .collect();

    let template = BackupsTemplate {
        backups: &backup_items,
        summary: &summary,
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render backups template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /api/backups -> JSON list of physical basebackup snapshots
pub async fn api_list_backups(
    State(state): State<Arc<DashboardState>>,
) -> Result<Json<Vec<BasebackupMeta>>, (StatusCode, String)> {
    state
        .backup_service
        .list_backups()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// POST /api/backups -> Triggers creation of a physical basebackup
pub async fn api_create_backup(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<CreateBackupRequest>,
) -> Result<Json<BasebackupMeta>, (StatusCode, String)> {
    let b_type = payload.backup_type.unwrap_or(BackupType::Full);
    let meta = state
        .backup_service
        .create_backup(b_type, payload.label)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    {
        let mut overview = state.overview.write().await;
        overview.total_backups += 1;
        overview.last_backup_at = Some(meta.created_at);
    }

    Ok(Json(meta))
}

/// GET /api/backups/:id/download -> Streams compressed basebackup archive (.tar.gz)
pub async fn api_download_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (meta, tar_bytes) = state
        .backup_service
        .get_backup_archive(&snapshot_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    let filename = format!("{}.tar.gz", meta.snapshot_id);
    let disposition = format!("attachment; filename=\"{}\"", filename);

    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "application/gzip".to_string(),
        ),
        (axum::http::header::CONTENT_DISPOSITION, disposition),
    ];

    Ok((headers, tar_bytes))
}

/// POST /api/backups/:id/restore -> Triggers restore of database from snapshot
pub async fn api_restore_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
    Json(payload): Json<RestoreBackupRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let msg = state
        .backup_service
        .restore_backup(&snapshot_id, payload.recovery_target_time)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "status": "success",
        "message": msg,
        "snapshot_id": snapshot_id
    })))
}

/// DELETE /api/backups/:id -> Deletes basebackup snapshot from storage
pub async fn api_delete_backup(
    State(state): State<Arc<DashboardState>>,
    Path(snapshot_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .backup_service
        .delete_backup(&snapshot_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    {
        let mut overview = state.overview.write().await;
        if overview.total_backups > 0 {
            overview.total_backups -= 1;
        }
    }

    Ok(Json(serde_json::json!({
        "status": "deleted",
        "snapshot_id": snapshot_id
    })))
}

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
