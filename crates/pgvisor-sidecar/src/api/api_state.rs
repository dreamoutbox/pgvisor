use std::sync::Arc;
use std::time::Duration;

use pgvisor_core::audit::AuditLog;
use pgvisor_core::sql_guard::SqlSecurityGuard;

use super::backup::{BackupRetentionConfig, LeaderRestoreExecutor, LocalLeaderRestoreExecutor};
use super::metrics::MetricsRing;
use super::password_verifier::{PasswordChecker, PostgresPasswordChecker};
use super::session_token::SessionTokenSigner;
use super::sql_runner::{ApiSqlRunner, LivePgSqlRunner};
use super::superusers::SuperuserAllowlist;
use crate::control::SidecarState;

const DEFAULT_PEER_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared application state for the client-facing `/api/*` Axum router.
#[derive(Clone)]
pub struct ApiState {
    pub sidecar: SidecarState,
    pub cluster_id: String,
    pub pg_addr: String,
    pub pg_superuser: String,
    pub superusers: Arc<SuperuserAllowlist>,
    pub token_signer: Arc<SessionTokenSigner>,
    pub password_checker: Arc<dyn PasswordChecker>,
    pub sql_runner: Arc<dyn ApiSqlRunner>,
    pub leader_restore_executor: Arc<dyn LeaderRestoreExecutor>,
    pub security_guard: Arc<SqlSecurityGuard>,
    pub metrics_ring: Arc<MetricsRing>,
    pub backup_retention: BackupRetentionConfig,
    pub audit_log: Arc<AuditLog>,
    pub http_client: reqwest::Client,
}

impl ApiState {
    /// Constructs [`ApiState`] using live PostgreSQL connections at `pg_addr`.
    pub fn new(
        sidecar: SidecarState,
        pg_addr: impl Into<String>,
        pg_superuser: impl Into<String>,
        superusers: Arc<SuperuserAllowlist>,
        token_signer: Arc<SessionTokenSigner>,
        audit_log: Arc<AuditLog>,
    ) -> Self {
        let pg_addr = pg_addr.into();
        let pg_superuser = pg_superuser.into();
        let password_checker = Arc::new(PostgresPasswordChecker::new(
            pg_addr.clone(),
            pg_superuser.clone(),
        ));
        let sql_runner = Arc::new(LivePgSqlRunner::new(pg_addr.clone()));
        let http_client = reqwest::Client::builder()
            .timeout(DEFAULT_PEER_HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            sidecar,
            cluster_id: "pgvisor-cluster".to_string(),
            pg_addr,
            pg_superuser,
            superusers,
            token_signer,
            password_checker,
            sql_runner,
            leader_restore_executor: Arc::new(LocalLeaderRestoreExecutor),
            security_guard: Arc::new(SqlSecurityGuard::default()),
            metrics_ring: Arc::new(MetricsRing::new()),
            backup_retention: BackupRetentionConfig::default(),
            audit_log,
            http_client,
        }
    }

    /// Sets the cluster identifier displayed by `/api/status`.
    pub fn with_cluster_id(mut self, cluster_id: impl Into<String>) -> Self {
        self.cluster_id = cluster_id.into();
        self
    }

    /// Sets backup retention and storage metadata reported/pruned by `/api/backups`.
    pub fn with_backup_retention(mut self, retention: BackupRetentionConfig) -> Self {
        self.backup_retention = retention;
        self
    }

    /// Overrides the local leader restore executor (for unit tests).
    pub fn with_leader_restore_executor(
        mut self,
        executor: Arc<dyn LeaderRestoreExecutor>,
    ) -> Self {
        self.leader_restore_executor = executor;
        self
    }

    /// Overrides the SQL runner implementation (for unit tests).
    pub fn with_sql_runner(mut self, runner: Arc<dyn ApiSqlRunner>) -> Self {
        self.sql_runner = runner;
        self
    }

    /// Overrides the SQL security guard policy.
    pub fn with_security_guard(mut self, guard: Arc<SqlSecurityGuard>) -> Self {
        self.security_guard = guard;
        self
    }

    /// Overrides the password checker implementation (for unit tests).
    pub fn with_password_checker(mut self, checker: Arc<dyn PasswordChecker>) -> Self {
        self.password_checker = checker;
        self
    }
}
