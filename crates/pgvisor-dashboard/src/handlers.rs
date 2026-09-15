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
    format_bytes, AlterRoleRequest, AuditEventView, AuditListResponse, AuditOverviewStats,
    BackupItemView, BackupOverviewSummary, BestBackupQuery, BestBackupResponse, ClusterOverview,
    ColumnInfo, CreateBackupRequest, CreateRoleRequest, NodeActionRequest, NodeActionResponse,
    NodeHealthState, NodeLifecycleAction, NodeRole, NodeSummary, PgRole, QuickRestoreRequest,
    QuickRestoreResponse, RestoreBackupRequest, RoleMembershipRequest, SqlQueryError,
    SqlQueryRequest, SqlQueryResult, SwitchoverRequest, SwitchoverResponse, TableDataResponse,
    TablePrivilege, TablePrivilegeKind, TablePrivilegeRequest, TableSummary,
};
use crate::security::{SecurityError, SqlSecurityGuard};
use crate::templates::{
    AuditTemplate, BackupsTemplate, LoginTemplate, NodesTemplate, OverviewTemplate, TablesTemplate,
    UsersTemplate,
};
use pgvisor_core::audit::{AuditEventKind, AuditLog};

pub use crate::metrics::{
    api_metrics_history, api_metrics_snapshot, MetricsService, StandaloneMetricsService,
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
                    "PostgreSQL 18.6 on x86_64-pc-linux-gnu, compiled by gcc, 64-bit".into(),
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

/// Helper to parse a target recovery timestamp flexibly.
pub fn parse_target_timestamp(raw: &str) -> Result<chrono::DateTime<Utc>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Recovery target timestamp cannot be empty".to_string());
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }

    if let Ok(dt) = chrono::DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%z") {
        return Ok(dt.with_timezone(&Utc));
    }

    let formats = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ];

    for fmt in &formats {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(chrono::DateTime::<Utc>::from_naive_utc_and_offset(
                naive, Utc,
            ));
        }
    }

    Err(format!(
        "Invalid timestamp '{}'. Expected format: YYYY-MM-DD HH:MM:SS (UTC) or ISO-8601 (e.g. 2026-09-14 03:00:00)",
        raw
    ))
}

/// Helper to select the best basebackup snapshot for forward Point-In-Time-Recovery.
/// In PostgreSQL PITR, physical basebackups cannot roll backward: only snapshots taken
/// ON OR BEFORE the target timestamp can replay WAL forward to reach the target.
pub fn find_best_backup_snapshot<'a>(
    backups: &'a [BasebackupMeta],
    target_time: chrono::DateTime<Utc>,
) -> Result<&'a BasebackupMeta, String> {
    if backups.is_empty() {
        return Err("No basebackup snapshots found in storage".to_string());
    }

    let mut eligible: Vec<&BasebackupMeta> = backups
        .iter()
        .filter(|b| b.created_at <= target_time)
        .collect();

    if eligible.is_empty() {
        let earliest = backups.iter().min_by_key(|b| b.created_at).unwrap();
        return Err(format!(
            "No basebackup snapshot found prior to target time {}. Earliest available snapshot was created at {} ({}). PostgreSQL forward recovery cannot roll backward.",
            target_time.format("%Y-%m-%d %H:%M:%S UTC"),
            earliest.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            earliest.snapshot_id
        ));
    }

    // Sort ascending, pick the latest snapshot before or at target time
    eligible.sort_by_key(|b| b.created_at);
    Ok(eligible.last().unwrap())
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
    async fn quick_restore(&self, target_time: &str) -> Result<(BasebackupMeta, String), String> {
        let parsed_target = parse_target_timestamp(target_time)?;
        let backups = self.list_backups().await?;
        let best = find_best_backup_snapshot(&backups, parsed_target)?.clone();
        let msg = self
            .restore_backup(&best.snapshot_id, Some(target_time.to_string()))
            .await?;
        Ok((best, msg))
    }
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

/// Abstraction for managing cluster lifecycle, node operations, and leader switchover.
#[async_trait::async_trait]
pub trait ClusterService: Send + Sync {
    async fn switchover(&self, target_node_id: u64) -> Result<SwitchoverResponse, String>;
    async fn start_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
    async fn stop_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
    async fn restart_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
}

/// In-memory cluster service for standalone testing or dashboard demo.
pub struct StandaloneClusterService {
    overview: Arc<RwLock<ClusterOverview>>,
}

impl StandaloneClusterService {
    pub fn new(overview: Arc<RwLock<ClusterOverview>>) -> Self {
        Self { overview }
    }
}

#[async_trait::async_trait]
impl ClusterService for StandaloneClusterService {
    async fn switchover(&self, target_node_id: u64) -> Result<SwitchoverResponse, String> {
        let mut ov = self.overview.write().await;
        let prev = ov.leader_id;
        for node in &mut ov.nodes {
            if node.node_id == target_node_id {
                node.role = NodeRole::Leader;
            } else if node.role == NodeRole::Leader {
                node.role = NodeRole::Standby;
            }
        }
        ov.leader_id = Some(target_node_id);
        Ok(SwitchoverResponse {
            status: "ok".into(),
            message: format!("Switched over leader to Node #{}", target_node_id),
            previous_leader_id: prev,
            new_leader_id: target_node_id,
        })
    }

    async fn start_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            if node.state == NodeHealthState::Healthy {
                return Err(format!("Node #{} is already running", node_id));
            }
            node.state = NodeHealthState::Healthy;
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} started successfully", node_id),
                node_id,
                action: NodeLifecycleAction::Start,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }

    async fn stop_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            if node.state == NodeHealthState::Stopped {
                return Err(format!("Node #{} is already stopped", node_id));
            }
            node.state = NodeHealthState::Stopped;
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} stopped cleanly", node_id),
                node_id,
                action: NodeLifecycleAction::Stop,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }

    async fn restart_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.state = NodeHealthState::Healthy;
            node.uptime_secs = 0;
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} restarted successfully", node_id),
                node_id,
                action: NodeLifecycleAction::Restart,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }
}

/// Abstraction for managing database roles and permissions.
#[async_trait::async_trait]
pub trait UserService: Send + Sync {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String>;
    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String>;
    async fn get_table_privileges(&self, role: &str) -> Result<Vec<TablePrivilege>, String>;
    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String>;
    async fn drop_role(&self, name: &str) -> Result<(), String>;
    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String>;
    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String>;
    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String>;
    async fn set_table_privilege(
        &self,
        role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String>;
}

/// In-memory mock user service for standalone dashboard testing and demo mode.
pub struct StandaloneUserService {
    roles: Arc<RwLock<Vec<PgRole>>>,
    table_privileges: Arc<RwLock<Vec<TablePrivilege>>>,
}

impl StandaloneUserService {
    pub fn new() -> Self {
        let initial_roles = vec![
            PgRole {
                rolname: "app_user".into(),
                rolcanlogin: true,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: 50,
                member_of: vec!["read_only_group".into()],
            },
            PgRole {
                rolname: "analytics_worker".into(),
                rolcanlogin: true,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: 10,
                member_of: vec![],
            },
            PgRole {
                rolname: "read_only_group".into(),
                rolcanlogin: false,
                rolcreatedb: false,
                rolcreaterole: false,
                rolreplication: false,
                rolsuper: false,
                rolconnlimit: -1,
                member_of: vec![],
            },
        ];

        let initial_privs = vec![TablePrivilege {
            table_name: "pgvisor_demo".into(),
            schema: "public".into(),
            select: true,
            insert: false,
            update: false,
            delete: false,
            truncate: false,
            references: false,
            trigger: false,
        }];

        Self {
            roles: Arc::new(RwLock::new(initial_roles)),
            table_privileges: Arc::new(RwLock::new(initial_privs)),
        }
    }
}

impl Default for StandaloneUserService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl UserService for StandaloneUserService {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String> {
        let mut list = self.roles.read().await.clone();
        list.sort_by(|a, b| a.rolname.cmp(&b.rolname));
        Ok(list)
    }

    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String> {
        let list = self.roles.read().await;
        Ok(list.iter().find(|r| r.rolname == name).cloned())
    }

    async fn get_table_privileges(&self, _role: &str) -> Result<Vec<TablePrivilege>, String> {
        Ok(self.table_privileges.read().await.clone())
    }

    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String> {
        let mut list = self.roles.write().await;
        if list.iter().any(|r| r.rolname == req.name) {
            return Err(format!("Role '{}' already exists", req.name));
        }
        list.push(PgRole {
            rolname: req.name.clone(),
            rolcanlogin: req.login,
            rolcreatedb: req.createdb,
            rolcreaterole: req.createrole,
            rolreplication: req.replication,
            rolsuper: false,
            rolconnlimit: req.connection_limit,
            member_of: vec![],
        });
        Ok(())
    }

    async fn drop_role(&self, name: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let prev = list.len();
        list.retain(|r| r.rolname != name);
        if list.len() == prev {
            Err(format!("Role '{}' not found", name))
        } else {
            Ok(())
        }
    }

    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let role = list
            .iter_mut()
            .find(|r| r.rolname == name)
            .ok_or_else(|| format!("Role '{}' not found", name))?;

        if let Some(l) = req.login {
            role.rolcanlogin = l;
        }
        if let Some(db) = req.createdb {
            role.rolcreatedb = db;
        }
        if let Some(cr) = req.createrole {
            role.rolcreaterole = cr;
        }
        if let Some(rep) = req.replication {
            role.rolreplication = rep;
        }
        if let Some(limit) = req.connection_limit {
            role.rolconnlimit = limit;
        }
        Ok(())
    }

    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let target = list
            .iter_mut()
            .find(|r| r.rolname == role)
            .ok_or_else(|| format!("Role '{}' not found", role))?;

        if !target.member_of.iter().any(|m| m == group_role) {
            target.member_of.push(group_role.to_string());
        }
        Ok(())
    }

    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        let mut list = self.roles.write().await;
        let target = list
            .iter_mut()
            .find(|r| r.rolname == role)
            .ok_or_else(|| format!("Role '{}' not found", role))?;

        target.member_of.retain(|m| m != group_role);
        Ok(())
    }

    async fn set_table_privilege(
        &self,
        _role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String> {
        let mut privs = self.table_privileges.write().await;
        let priv_idx = if let Some(idx) = privs.iter().position(|p| p.table_name == req.table_name)
        {
            idx
        } else {
            privs.push(TablePrivilege {
                table_name: req.table_name.clone(),
                schema: req.schema.clone().unwrap_or_else(|| "public".into()),
                select: false,
                insert: false,
                update: false,
                delete: false,
                truncate: false,
                references: false,
                trigger: false,
            });
            privs.len().saturating_sub(1)
        };

        if let Some(entry) = privs.get_mut(priv_idx) {
            match req.privilege {
                TablePrivilegeKind::Select => entry.select = req.grant,
                TablePrivilegeKind::Insert => entry.insert = req.grant,
                TablePrivilegeKind::Update => entry.update = req.grant,
                TablePrivilegeKind::Delete => entry.delete = req.grant,
                TablePrivilegeKind::Truncate => entry.truncate = req.grant,
                TablePrivilegeKind::References => entry.references = req.grant,
                TablePrivilegeKind::Trigger => entry.trigger = req.grant,
            }
        }
        Ok(())
    }
}

/// Live PostgreSQL user service executing safe DDL queries against the leader node.
pub struct SqlUserService {
    sql_executor: Arc<dyn SqlExecutor>,
}

impl SqlUserService {
    pub fn new(sql_executor: Arc<dyn SqlExecutor>) -> Self {
        Self { sql_executor }
    }

    fn validate_identifier(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("Identifier cannot be empty".into());
        }
        if name.len() > 63 {
            return Err("Identifier exceeds maximum length of 63 characters".into());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("Identifier '{}' contains invalid characters", name));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl UserService for SqlUserService {
    async fn list_roles(&self) -> Result<Vec<PgRole>, String> {
        let sql = "SELECT r.rolname, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole, r.rolreplication, r.rolsuper, r.rolconnlimit, COALESCE((SELECT string_agg(b.rolname, ',') FROM pg_auth_members m JOIN pg_roles b ON (m.roleid = b.oid) WHERE m.member = r.oid), '') as member_of FROM pg_roles r WHERE r.rolname NOT LIKE 'pg_%' AND r.rolname != 'postgres' ORDER BY r.rolname;";
        let res = self.sql_executor.execute(sql, 200).await?;

        let roles = res
            .rows
            .into_iter()
            .filter_map(|row| {
                if row.len() >= 8 {
                    let rolname = row[0].clone();
                    let rolcanlogin =
                        row[1].eq_ignore_ascii_case("t") || row[1].eq_ignore_ascii_case("true");
                    let rolcreatedb =
                        row[2].eq_ignore_ascii_case("t") || row[2].eq_ignore_ascii_case("true");
                    let rolcreaterole =
                        row[3].eq_ignore_ascii_case("t") || row[3].eq_ignore_ascii_case("true");
                    let rolreplication =
                        row[4].eq_ignore_ascii_case("t") || row[4].eq_ignore_ascii_case("true");
                    let rolsuper =
                        row[5].eq_ignore_ascii_case("t") || row[5].eq_ignore_ascii_case("true");
                    let rolconnlimit = row[6].parse::<i32>().unwrap_or(-1);
                    let member_of = if row[7].is_empty() {
                        Vec::new()
                    } else {
                        row[7]
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    };

                    Some(PgRole {
                        rolname,
                        rolcanlogin,
                        rolcreatedb,
                        rolcreaterole,
                        rolreplication,
                        rolsuper,
                        rolconnlimit,
                        member_of,
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(roles)
    }

    async fn get_role(&self, name: &str) -> Result<Option<PgRole>, String> {
        Self::validate_identifier(name)?;
        let roles = self.list_roles().await?;
        Ok(roles.into_iter().find(|r| r.rolname == name))
    }

    async fn get_table_privileges(&self, role: &str) -> Result<Vec<TablePrivilege>, String> {
        Self::validate_identifier(role)?;
        let sql = format!(
            "SELECT t.table_name, has_table_privilege('{}', quote_ident(t.table_name), 'SELECT') as can_select, has_table_privilege('{}', quote_ident(t.table_name), 'INSERT') as can_insert, has_table_privilege('{}', quote_ident(t.table_name), 'UPDATE') as can_update, has_table_privilege('{}', quote_ident(t.table_name), 'DELETE') as can_delete, has_table_privilege('{}', quote_ident(t.table_name), 'TRUNCATE') as can_truncate, has_table_privilege('{}', quote_ident(t.table_name), 'REFERENCES') as can_references, has_table_privilege('{}', quote_ident(t.table_name), 'TRIGGER') as can_trigger FROM information_schema.tables t WHERE t.table_schema = 'public' ORDER BY t.table_name;",
            role, role, role, role, role, role, role
        );

        let res = self.sql_executor.execute(&sql, 200).await?;
        let privs = res
            .rows
            .into_iter()
            .filter_map(|row| {
                if row.len() >= 8 {
                    let parse_bool =
                        |s: &str| s.eq_ignore_ascii_case("t") || s.eq_ignore_ascii_case("true");
                    Some(TablePrivilege {
                        table_name: row[0].clone(),
                        schema: "public".into(),
                        select: parse_bool(&row[1]),
                        insert: parse_bool(&row[2]),
                        update: parse_bool(&row[3]),
                        delete: parse_bool(&row[4]),
                        truncate: parse_bool(&row[5]),
                        references: parse_bool(&row[6]),
                        trigger: parse_bool(&row[7]),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(privs)
    }

    async fn create_role(&self, req: &CreateRoleRequest) -> Result<(), String> {
        Self::validate_identifier(&req.name)?;
        let mut ddl = format!(
            "CREATE ROLE \"{}\" WITH {} {} {} {} CONNECTION LIMIT {}",
            req.name,
            if req.login { "LOGIN" } else { "NOLOGIN" },
            if req.createdb {
                "CREATEDB"
            } else {
                "NOCREATEDB"
            },
            if req.createrole {
                "CREATEROLE"
            } else {
                "NOCREATEROLE"
            },
            if req.replication {
                "REPLICATION"
            } else {
                "NOREPLICATION"
            },
            req.connection_limit
        );

        if let Some(pwd) = req.password.as_deref() {
            if !pwd.is_empty() {
                let escaped_pwd = pwd.replace('\'', "''");
                ddl.push_str(&format!(" PASSWORD '{}'", escaped_pwd));
            }
        }
        ddl.push(';');

        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn drop_role(&self, name: &str) -> Result<(), String> {
        Self::validate_identifier(name)?;
        let ddl = format!("DROP ROLE \"{}\";", name);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn alter_role(&self, name: &str, req: &AlterRoleRequest) -> Result<(), String> {
        Self::validate_identifier(name)?;
        let mut clauses = Vec::new();
        if let Some(login) = req.login {
            clauses.push(if login { "LOGIN" } else { "NOLOGIN" });
        }
        if let Some(createdb) = req.createdb {
            clauses.push(if createdb { "CREATEDB" } else { "NOCREATEDB" });
        }
        if let Some(createrole) = req.createrole {
            clauses.push(if createrole {
                "CREATEROLE"
            } else {
                "NOCREATEROLE"
            });
        }
        if let Some(replication) = req.replication {
            clauses.push(if replication {
                "REPLICATION"
            } else {
                "NOREPLICATION"
            });
        }
        let limit_str;
        if let Some(conn_limit) = req.connection_limit {
            limit_str = format!("CONNECTION LIMIT {}", conn_limit);
            clauses.push(&limit_str);
        }
        let pwd_str;
        if let Some(pwd) = req.password.as_deref() {
            if !pwd.is_empty() {
                let escaped_pwd = pwd.replace('\'', "''");
                pwd_str = format!("PASSWORD '{}'", escaped_pwd);
                clauses.push(&pwd_str);
            }
        }

        if clauses.is_empty() {
            return Ok(());
        }

        let ddl = format!("ALTER ROLE \"{}\" WITH {};", name, clauses.join(" "));
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn grant_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(group_role)?;
        let ddl = format!("GRANT \"{}\" TO \"{}\";", group_role, role);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn revoke_membership(&self, role: &str, group_role: &str) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(group_role)?;
        let ddl = format!("REVOKE \"{}\" FROM \"{}\";", group_role, role);
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }

    async fn set_table_privilege(
        &self,
        role: &str,
        req: &TablePrivilegeRequest,
    ) -> Result<(), String> {
        Self::validate_identifier(role)?;
        Self::validate_identifier(&req.table_name)?;
        let schema = req.schema.as_deref().unwrap_or("public");
        Self::validate_identifier(schema)?;

        let verb = if req.grant { "GRANT" } else { "REVOKE" };
        let preposition = if req.grant { "TO" } else { "FROM" };
        let ddl = format!(
            "{} {} ON TABLE \"{}\".\"{}\" {} \"{}\";",
            verb,
            req.privilege.as_sql_str(),
            schema,
            req.table_name,
            preposition,
            role
        );
        self.sql_executor.execute(&ddl, 1).await.map(|_| ())
    }
}

/// Shared application state for dashboard HTTP handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub overview: Arc<RwLock<ClusterOverview>>,
    pub security_guard: Arc<SqlSecurityGuard>,
    pub sql_executor: Arc<dyn SqlExecutor>,
    pub backup_service: Arc<dyn BackupService>,
    pub cluster_service: Arc<dyn ClusterService>,
    pub user_service: Arc<dyn UserService>,
    pub metrics_service: Arc<dyn MetricsService>,
    pub audit_log: Arc<AuditLog>,
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
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 0,
                    uptime_secs: 3600,
                    is_local: true,
                },
                NodeSummary {
                    node_id: 2,
                    address: "127.0.0.1:5433".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 64,
                    uptime_secs: 3580,
                    is_local: false,
                },
                NodeSummary {
                    node_id: 3,
                    address: "127.0.0.1:5434".into(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "18.6".into(),
                    replication_lag_bytes: 128,
                    uptime_secs: 3550,
                    is_local: false,
                },
            ],
        };

        let overview_arc = Arc::new(RwLock::new(initial_overview));
        Self {
            overview: overview_arc.clone(),
            security_guard: Arc::new(SqlSecurityGuard::default()),
            sql_executor: Arc::new(StandaloneSqlExecutor),
            backup_service: Arc::new(StandaloneBackupService::new()),
            cluster_service: Arc::new(StandaloneClusterService::new(overview_arc)),
            user_service: Arc::new(StandaloneUserService::new()),
            metrics_service: Arc::new(StandaloneMetricsService::new()),
            audit_log: Arc::new(AuditLog::new(cluster_id, None, 2000)),
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

/// GET /api/nodes -> Returns list of cluster nodes summary JSON
pub async fn api_nodes(State(state): State<Arc<DashboardState>>) -> Json<Vec<NodeSummary>> {
    let overview = state.overview.read().await;
    Json(overview.nodes.clone())
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

/// POST /api/backups/quick-restore -> Resolves best basebackup snapshot and restores to target time
pub async fn api_quick_restore(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<QuickRestoreRequest>,
) -> Result<Json<QuickRestoreResponse>, (StatusCode, String)> {
    let (best, msg) = state
        .backup_service
        .quick_restore(&payload.recovery_target_time)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    Ok(Json(QuickRestoreResponse {
        status: "success".to_string(),
        message: msg,
        snapshot_id: best.snapshot_id,
        recovery_target_time: payload.recovery_target_time,
        snapshot_created_at: best.created_at.to_rfc3339(),
    }))
}

/// GET /api/backups/best?target_time=... -> Returns information on the best snapshot for a target time
pub async fn api_find_best_backup(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<BestBackupQuery>,
) -> Result<Json<BestBackupResponse>, (StatusCode, String)> {
    let parsed_target =
        parse_target_timestamp(&query.target_time).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let backups = state
        .backup_service
        .list_backups()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let best = find_best_backup_snapshot(&backups, parsed_target)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let backup_type = match best.backup_type {
        BackupType::Full => "full".to_string(),
        BackupType::Incremental => "incremental".to_string(),
    };

    Ok(Json(BestBackupResponse {
        snapshot_id: best.snapshot_id.clone(),
        created_at: best.created_at.to_rfc3339(),
        backup_type,
        label: best.label.clone(),
    }))
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

/// POST /api/cluster/switchover -> Initiates manual leader switchover to target node
pub async fn api_switchover(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<SwitchoverRequest>,
) -> Result<Json<SwitchoverResponse>, (StatusCode, Json<serde_json::Value>)> {
    let target_node_id = payload.target_node_id;

    // Validate target node exists and is a healthy standby
    {
        let overview = state.overview.read().await;
        let target_node = overview.nodes.iter().find(|n| n.node_id == target_node_id);

        match target_node {
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("Target node #{} does not exist in cluster", target_node_id)
                    })),
                ));
            }
            Some(node) => {
                if node.role == NodeRole::Leader {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("Node #{} is already the active leader", target_node_id)
                        })),
                    ));
                }
                if node.state != NodeHealthState::Healthy {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("Target node #{} is not healthy (current state: {:?})", target_node_id, node.state)
                        })),
                    ));
                }
            }
        }
    }

    info!(target_node_id, "Initiating cluster leader switchover");

    let resp = state
        .cluster_service
        .switchover(target_node_id)
        .await
        .map_err(|e| {
            error!(target_node_id, ?e, "Switchover failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Switchover failed: {}", e)
                })),
            )
        })?;

    // Update in-memory overview
    {
        let mut overview = state.overview.write().await;
        overview.leader_id = Some(resp.new_leader_id);
        for node in &mut overview.nodes {
            if node.node_id == resp.new_leader_id {
                node.role = NodeRole::Leader;
            } else if Some(node.node_id) == resp.previous_leader_id {
                node.role = NodeRole::Standby;
            }
        }
    }

    Ok(Json(resp))
}

async fn api_node_action_internal(
    state: Arc<DashboardState>,
    node_id: u64,
    action: NodeLifecycleAction,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    // Validate target node exists in cluster overview
    {
        let overview = state.overview.read().await;
        if !overview.nodes.iter().any(|n| n.node_id == node_id) {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Node #{} not found in cluster", node_id)
                })),
            ));
        }
    }

    info!(node_id, action = ?action, "Executing node lifecycle action from dashboard API");

    let res = match action {
        NodeLifecycleAction::Start => state.cluster_service.start_node(node_id).await,
        NodeLifecycleAction::Stop => state.cluster_service.stop_node(node_id).await,
        NodeLifecycleAction::Restart => state.cluster_service.restart_node(node_id).await,
    };

    match res {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            error!(node_id, action = ?action, ?e, "Node lifecycle action failed");
            let status = if e.contains("already") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err((
                status,
                Json(serde_json::json!({
                    "error": e
                })),
            ))
        }
    }
}

/// POST /api/nodes/:node_id/start -> Starts PostgreSQL on target node
pub async fn api_start_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Start).await
}

/// POST /api/nodes/:node_id/stop -> Stops PostgreSQL on target node
pub async fn api_stop_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Stop).await
}

/// POST /api/nodes/:node_id/restart -> Restarts PostgreSQL on target node
pub async fn api_restart_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Restart).await
}

/// POST /api/nodes/:node_id/action -> Executes arbitrary lifecycle action on target node
pub async fn api_node_action(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
    Json(payload): Json<NodeActionRequest>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, payload.action).await
}

/// Query parameters for the /users dashboard view.
#[derive(Debug, Default, Deserialize)]
pub struct UsersQuery {
    pub role: Option<String>,
    pub tab: Option<String>,
}

/// GET /users -> Renders database users and role permissions management page.
pub async fn get_users_page(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<UsersQuery>,
) -> Result<Html<String>, StatusCode> {
    let roles = state.user_service.list_roles().await.unwrap_or_default();
    let active_role_name = params
        .role
        .as_deref()
        .or_else(|| roles.first().map(|r| r.rolname.as_str()));
    let active_role = active_role_name.and_then(|name| roles.iter().find(|r| r.rolname == name));
    let active_tab = params.tab.as_deref().unwrap_or("attributes");

    let table_privileges = if let Some(r) = active_role {
        state
            .user_service
            .get_table_privileges(&r.rolname)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let all_tables = api_list_tables(State(state.clone())).await.0;

    let template = UsersTemplate {
        roles: &roles,
        active_role,
        active_tab,
        table_privileges: &table_privileges,
        all_tables: &all_tables,
        all_roles: &roles,
        auth_enabled: state.admin_token.is_some(),
    };

    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render users template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /api/users -> List all database roles (excluding system pg_* and postgres roles)
pub async fn api_list_users(State(state): State<Arc<DashboardState>>) -> Json<Vec<PgRole>> {
    let roles = state.user_service.list_roles().await.unwrap_or_default();
    Json(roles)
}

/// GET /api/users/:role -> Get details of a single database role
pub async fn api_get_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<PgRole>, (StatusCode, String)> {
    match state.user_service.get_role(&role).await {
        Ok(Some(r)) => Ok(Json(r)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Role '{}' not found", role))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/users -> Create a new database role
pub async fn api_create_user(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<CreateRoleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("CREATE ROLE \"{}\" WITH LOGIN;", req.name);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .create_role(&req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!(
                "Database role '{}' created (login={}, createdb={}, createrole={})",
                req.name, req.login, req.createdb, req.createrole
            ),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' created successfully", req.name)
    })))
}

/// DELETE /api/users/:role -> Drop an existing database role
pub async fn api_drop_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("DROP ROLE \"{}\";", role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .drop_role(&role)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Database role '{}' dropped", role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' dropped successfully", role)
    })))
}

/// PUT /api/users/:role -> Alter attributes of an existing database role
pub async fn api_alter_user(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<AlterRoleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("ALTER ROLE \"{}\" WITH LOGIN;", role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .alter_role(&role, &req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Database role '{}' attributes altered", role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Role '{}' updated successfully", role)
    })))
}

/// POST /api/users/:role/memberships -> Grant group membership to a role
pub async fn api_grant_membership(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<RoleMembershipRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("GRANT \"{}\" TO \"{}\";", req.group_role, role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .grant_membership(&role, &req.group_role)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Granted group '{}' to role '{}'", req.group_role, role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Granted group '{}' to role '{}'", req.group_role, role)
    })))
}

/// DELETE /api/users/:role/memberships/:group -> Revoke group membership from a role
pub async fn api_revoke_membership(
    State(state): State<Arc<DashboardState>>,
    Path((role, group)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ddl_check = format!("REVOKE \"{}\" FROM \"{}\";", group, role);
    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .revoke_membership(&role, &group)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!("Revoked group '{}' from role '{}'", group, role),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Revoked group '{}' from role '{}'", group, role)
    })))
}

/// GET /api/users/:role/privileges -> List table privileges for a role
pub async fn api_get_privileges(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
) -> Result<Json<Vec<TablePrivilege>>, (StatusCode, String)> {
    let privs = state
        .user_service
        .get_table_privileges(&role)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(privs))
}

/// POST /api/users/:role/privileges -> Grant or revoke a table privilege for a role
pub async fn api_set_privilege(
    State(state): State<Arc<DashboardState>>,
    Path(role): Path<String>,
    Json(req): Json<TablePrivilegeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let verb = if req.grant { "GRANT" } else { "REVOKE" };
    let prep = if req.grant { "TO" } else { "FROM" };
    let ddl_check = format!(
        "{} {} ON TABLE public.\"{}\" {} \"{}\";",
        verb,
        req.privilege.as_sql_str(),
        req.table_name,
        prep,
        role
    );

    state
        .security_guard
        .validate_user_management_ddl(&ddl_check)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    state
        .user_service
        .set_table_privilege(&role, &req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    state
        .audit_log
        .append(
            AuditEventKind::UserPermission,
            None,
            None,
            format!(
                "{} table privilege {} on '{}' {} role '{}'",
                if req.grant { "Granted" } else { "Revoked" },
                req.privilege.as_sql_str(),
                req.table_name,
                if req.grant { "to" } else { "from" },
                role
            ),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!(
            "{} {} on {} {} role {}",
            if req.grant { "Granted" } else { "Revoked" },
            req.privilege.as_sql_str(),
            req.table_name,
            if req.grant { "to" } else { "from" },
            role
        )
    })))
}

/// Query parameters for listing and searching audit events.
#[derive(Deserialize, Default)]
pub struct AuditQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /audit-logs -> Renders Audit Logs HTML dashboard page
pub async fn get_audit_logs_page(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<AuditQuery>,
) -> Result<Html<String>, StatusCode> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;

    let target_kind = query
        .kind
        .as_deref()
        .and_then(AuditEventKind::from_snake_case);
    let (events, total) = state
        .audit_log
        .list(limit, offset, target_kind, query.q.as_deref())
        .await;

    let total_pages = if total == 0 {
        1
    } else {
        (total + limit - 1) / limit
    };

    let (total_events, dangerous_sql_count, latest_pitr_target) = state.audit_log.stats().await;
    let stats = AuditOverviewStats {
        total_events,
        dangerous_sql_count,
        latest_pitr_target,
    };

    let views: Vec<AuditEventView> = events
        .into_iter()
        .map(|e| AuditEventView {
            id: e.id,
            occurred_at: e.occurred_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            kind: e.kind.as_str().to_string(),
            kind_display: e.kind.display_name().to_string(),
            node_id: e.node_id,
            node_address: e.node_address,
            detail: e.detail,
            pitr_target: e.pitr_target,
        })
        .collect();

    let template = AuditTemplate {
        events: &views,
        stats: &stats,
        active_kind: query.kind.as_deref(),
        search_query: query.q.as_deref(),
        page,
        limit,
        total_pages,
        total_events: total,
        auth_enabled: state.admin_token.is_some(),
    };

    template
        .render()
        .map(Html)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// GET /api/audit-logs -> Returns paginated JSON audit events for monitoring and automated verification
pub async fn api_list_audit_logs(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<AuditQuery>,
) -> Json<AuditListResponse> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or_else(|| {
        let p = query.page.unwrap_or(1).max(1);
        (p - 1) * limit
    });

    let target_kind = query
        .kind
        .as_deref()
        .and_then(AuditEventKind::from_snake_case);
    let (events, total) = state
        .audit_log
        .list(limit, offset, target_kind, query.q.as_deref())
        .await;

    let (_total_events, dangerous_sql_count, latest_pitr_target) = state.audit_log.stats().await;

    let views: Vec<AuditEventView> = events
        .into_iter()
        .map(|e| AuditEventView {
            id: e.id,
            occurred_at: e.occurred_at.to_rfc3339(),
            kind: e.kind.as_str().to_string(),
            kind_display: e.kind.display_name().to_string(),
            node_id: e.node_id,
            node_address: e.node_address,
            detail: e.detail,
            pitr_target: e.pitr_target,
        })
        .collect();

    Json(AuditListResponse {
        events: views,
        total,
        limit,
        offset,
        dangerous_sql_count,
        latest_pitr_target,
    })
}
