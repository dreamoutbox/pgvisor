use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use pgvisor_core::node::{
    LogLevel, NodeConfigResponse, NodeConfigType, NodeLogEntry,
};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::{ConfigError, ConfigGenerator, PostgresConfig};

pub const MAX_LOG_ENTRIES: usize = 2000;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Command execution failed: {0}")]
    CommandFailed(String),

    #[error("Postgres process not currently running")]
    NotRunning,
}

/// Operational state of the supervised PostgreSQL child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Stopped,
    Running,
    Fenced,
    Restoring,
}

/// Container PID 1 supervisor managing Postgres lifecycle, signal routing, and fencing.
pub struct PostgresSupervisor {
    data_dir: PathBuf,
    status: Arc<Mutex<ProcessStatus>>,
    child_pid: Arc<AtomicU32>,
    active_child: Arc<Mutex<Option<Child>>>,
    started_at: Arc<Mutex<Option<std::time::Instant>>>,
    logs: Arc<std::sync::RwLock<VecDeque<NodeLogEntry>>>,
}

impl PostgresSupervisor {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self::with_log_buffer(
            data_dir,
            Arc::new(std::sync::RwLock::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
        )
    }

    pub fn with_log_buffer(
        data_dir: impl AsRef<Path>,
        logs: Arc<std::sync::RwLock<VecDeque<NodeLogEntry>>>,
    ) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            status: Arc::new(Mutex::new(ProcessStatus::Stopped)),
            child_pid: Arc::new(AtomicU32::new(0)),
            active_child: Arc::new(Mutex::new(None)),
            started_at: Arc::new(Mutex::new(None)),
            logs,
        }
    }

    /// Ensures data directory is initialized.
    /// If primary_conninfo is set and data_dir is uninitialized, clones from primary via pg_basebackup.
    /// Otherwise initializes a primary cluster via initdb.
    pub async fn ensure_initialized(
        &self,
        superuser: &str,
        primary_conninfo: Option<&str>,
    ) -> Result<(), SupervisorError> {
        let version_file = self.data_dir.join("PG_VERSION");
        if version_file.exists() {
            info!(dir = ?self.data_dir, "Existing PostgreSQL cluster detected, skipping initialization");
            return Ok(());
        }

        if let Some(conninfo) = primary_conninfo {
            info!(dir = ?self.data_dir, conninfo, "Standby node: waiting for primary to be ready for pg_basebackup");
            let mut retries = 30;
            while retries > 0 {
                let status = Command::new("pg_isready")
                    .arg("-d")
                    .arg(conninfo)
                    .status()
                    .await;
                if let Ok(s) = status {
                    if s.success() {
                        info!("Primary is ready, initiating pg_basebackup clone");
                        break;
                    }
                }
                retries -= 1;
                if retries == 0 {
                    return Err(SupervisorError::CommandFailed(
                        "Primary did not become ready in time for replication".into(),
                    ));
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }

            let status = Command::new("pg_basebackup")
                .arg("-d")
                .arg(conninfo)
                .arg("-D")
                .arg(&self.data_dir)
                .arg("-Fp")
                .arg("-Xs")
                .arg("-R")
                .status()
                .await?;

            if !status.success() {
                return Err(SupervisorError::CommandFailed(format!(
                    "pg_basebackup failed with status: {status}"
                )));
            }
            info!("pg_basebackup clone completed successfully");
        } else {
            info!(dir = ?self.data_dir, superuser, "Running initdb to initialize new cluster");
            let status = Command::new("initdb")
                .arg("-D")
                .arg(&self.data_dir)
                .arg("-U")
                .arg(superuser)
                .arg("-A")
                .arg("trust")
                .status()
                .await?;

            if !status.success() {
                return Err(SupervisorError::CommandFailed(format!(
                    "initdb failed with status: {status}"
                )));
            }
            info!("initdb completed successfully");
        }

        Ok(())
    }

    /// Initializes data directory if PG_VERSION is absent (convenience wrapper for primaries).
    pub async fn ensure_initdb(&self, superuser: &str) -> Result<(), SupervisorError> {
        self.ensure_initialized(superuser, None).await
    }

    /// Generates configurations and starts the PostgreSQL child process.
    pub async fn start(&self, config: &PostgresConfig) -> Result<(), SupervisorError> {
        {
            let st = self.status.lock().await;
            if *st == ProcessStatus::Running {
                info!(dir = ?self.data_dir, "PostgreSQL process is already running");
                return Ok(());
            }
        }

        if *self.status.lock().await != ProcessStatus::Restoring {
            pgvisor_core::log_highlight("START NODE");
        }

        ConfigGenerator::write_configs(&self.data_dir, config)?;

        info!(dir = ?self.data_dir, "Spawning postgres process");
        let mut cmd = Command::new("postgres");
        cmd.arg("-D")
            .arg(&self.data_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        self.child_pid.store(pid, Ordering::SeqCst);

        // Pipe stdout and stderr to tracing logs which will be captured into the in-memory circular buffer
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    info!(target: "postgres", "{}", line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    warn!(target: "postgres", "{}", line);
                }
            });
        }

        {
            let mut active = self.active_child.lock().await;
            *active = Some(child);
            let mut st = self.status.lock().await;
            if *st != ProcessStatus::Restoring {
                *st = ProcessStatus::Running;
            }
            let mut started = self.started_at.lock().await;
            *started = Some(std::time::Instant::now());
        }

        info!(pid, "PostgreSQL process running under sidecar supervision");
        Ok(())
    }

    /// Promotes a standby replica to read-write primary via `pg_ctl promote`.
    pub async fn promote(&self) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, "Executing pg_ctl promote");
        let output = Command::new("pg_ctl")
            .arg("promote")
            .arg("-D")
            .arg(&self.data_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not in standby mode") {
                info!("PostgreSQL instance is already operating as primary");
                return Ok(());
            }
            return Err(SupervisorError::CommandFailed(format!(
                "pg_ctl promote failed with status: {} ({})",
                output.status, stderr
            )));
        }

        // Clean up postgresql.auto.conf and standby.signal to ensure clean primary state
        let auto_conf = self.data_dir.join("postgresql.auto.conf");
        if auto_conf.exists() {
            let _ = tokio::fs::remove_file(&auto_conf).await;
        }
        let standby_signal = self.data_dir.join("standby.signal");
        if standby_signal.exists() {
            let _ = tokio::fs::remove_file(&standby_signal).await;
        }

        info!("PostgreSQL instance promoted to leader successfully");
        Ok(())
    }

    /// Re-points a standby replica's primary_conninfo to a new primary and reloads config.
    pub async fn repoint_primary(&self, new_primary_conninfo: &str) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, conninfo = %new_primary_conninfo, "Re-pointing standby replica to new primary");
        let auto_conf_path = self.data_dir.join("postgresql.auto.conf");
        let line = format!("primary_conninfo = '{}'\n", new_primary_conninfo);

        // Ensure standby.signal exists for replica operation
        let signal_path = self.data_dir.join("standby.signal");
        if !signal_path.exists() {
            let _ = tokio::fs::File::create(&signal_path).await;
        }

        tokio::fs::write(&auto_conf_path, line.as_bytes()).await?;

        // Signal reload to running PostgreSQL WAL receiver only if Postgres is currently running
        let is_running = {
            let st = self.status.lock().await;
            *st == ProcessStatus::Running
        };

        if is_running {
            let status = Command::new("pg_ctl")
                .arg("reload")
                .arg("-D")
                .arg(&self.data_dir)
                .status()
                .await;

            if let Ok(s) = status {
                if s.success() {
                    info!("PostgreSQL primary_conninfo reloaded successfully");
                    return Ok(());
                }
            }

            warn!("pg_ctl reload did not succeed cleanly, checking process status");
        } else {
            info!(dir = ?self.data_dir, "PostgreSQL is not running, skipping pg_ctl reload");
        }
        Ok(())
    }

    /// Immediately halts Postgres using `pg_ctl stop -m immediate` to prevent split-brain writes.
    pub async fn fence(&self) -> Result<(), SupervisorError> {
        pgvisor_core::log_highlight("STOP NODE");
        warn!(dir = ?self.data_dir, "FENCING: executing immediate stop on Postgres");
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Fenced;
            let mut started = self.started_at.lock().await;
            *started = None;
        }

        if self.data_dir.join("PG_VERSION").exists() {
            let status = Command::new("pg_ctl")
                .arg("stop")
                .arg("-D")
                .arg(&self.data_dir)
                .arg("-m")
                .arg("immediate")
                .status()
                .await;

            match status {
                Ok(s) if s.success() => {
                    info!("Postgres stopped immediately via pg_ctl");
                    let mut active = self.active_child.lock().await;
                    if let Some(mut child) = active.take() {
                        let _ = child.wait().await;
                    }
                }
                _ => {
                    // If pg_ctl fails, kill child process directly
                    let mut active = self.active_child.lock().await;
                    if let Some(mut child) = active.take() {
                        let _ = child.kill().await;
                        warn!("Forcefully killed Postgres child process");
                    }
                }
            }
        } else {
            let mut active = self.active_child.lock().await;
            if let Some(mut child) = active.take() {
                let _ = child.kill().await;
                warn!("Forcefully killed Postgres child process");
            }
        }

        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Gracefully stops Postgres child process using `pg_ctl stop -m fast`.
    pub async fn stop(&self) -> Result<(), SupervisorError> {
        {
            let st = self.status.lock().await;
            if *st == ProcessStatus::Stopped {
                info!(dir = ?self.data_dir, "PostgreSQL process is already stopped");
                return Ok(());
            }
        }

        if *self.status.lock().await != ProcessStatus::Restoring {
            pgvisor_core::log_highlight("STOP NODE");
        }

        info!(dir = ?self.data_dir, "Executing fast shutdown on Postgres");
        if self.data_dir.join("PG_VERSION").exists() {
            let status = Command::new("pg_ctl")
                .arg("stop")
                .arg("-D")
                .arg(&self.data_dir)
                .arg("-m")
                .arg("fast")
                .status()
                .await;

            match status {
                Ok(s) if s.success() => {
                    info!("Postgres stopped cleanly");
                    let mut active = self.active_child.lock().await;
                    if let Some(mut child) = active.take() {
                        let _ = child.wait().await;
                    }
                }
                _ => {
                    let mut active = self.active_child.lock().await;
                    if let Some(mut child) = active.take() {
                        let _ = child.kill().await;
                    }
                }
            }
        } else {
            let mut active = self.active_child.lock().await;
            if let Some(mut child) = active.take() {
                let _ = child.kill().await;
            }
        }

        let mut st = self.status.lock().await;
        if *st != ProcessStatus::Restoring {
            *st = ProcessStatus::Stopped;
        }
        let mut started = self.started_at.lock().await;
        *started = None;
        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Gracefully restarts the PostgreSQL child process under sidecar supervision.
    pub async fn restart(&self, config: &PostgresConfig) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, "Restarting PostgreSQL process under sidecar supervision");
        self.stop().await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        self.start(config).await?;
        self.wait_ready(config.port, 30).await?;
        info!("PostgreSQL restarted successfully and accepting connections");
        Ok(())
    }

    /// Returns the current supervisor process status.
    pub async fn status(&self) -> ProcessStatus {
        let st = self.status.lock().await;
        *st
    }

    /// Updates supervisor process status.
    pub async fn set_status(&self, status: ProcessStatus) {
        let mut st = self.status.lock().await;
        *st = status;
    }

    /// Returns the monitored child PID, or 0 if not running.
    pub fn child_pid(&self) -> u32 {
        self.child_pid.load(Ordering::SeqCst)
    }

    /// Returns the uptime of the monitored Postgres process in seconds.
    pub async fn uptime_secs(&self) -> u64 {
        let started = self.started_at.lock().await;
        started.map(|t| t.elapsed().as_secs()).unwrap_or(0)
    }

    /// Checks whether the PostgreSQL child process is actively running.
    pub async fn is_running(&self) -> bool {
        let mut active = self.active_child.lock().await;
        if let Some(child) = active.as_mut() {
            match child.try_wait() {
                Ok(None) => true,
                _ => false,
            }
        } else {
            false
        }
    }

    /// Waits for child process to terminate.
    pub async fn wait(&self) -> Option<ExitStatus> {
        let mut active = self.active_child.lock().await;
        if let Some(child) = active.as_mut() {
            match child.wait().await {
                Ok(status) => {
                    info!(?status, "Postgres child process exited");
                    let mut st = self.status.lock().await;
                    if *st != ProcessStatus::Fenced {
                        *st = ProcessStatus::Stopped;
                    }
                    Some(status)
                }
                Err(err) => {
                    error!(%err, "Error waiting on Postgres child");
                    None
                }
            }
        } else {
            None
        }
    }

    /// Returns the staging data directory path used for shadow restore.
    ///
    /// Sibling directory `<data_dir>_staging` located in the same parent directory as `data_dir`
    /// to guarantee that swapping directories after recovery is an atomic rename on the same volume.
    ///
    /// # Example
    /// ```no_run
    /// use std::path::Path;
    /// use pgvisor_sidecar::supervisor::PostgresSupervisor;
    ///
    /// let supervisor = PostgresSupervisor::new("/var/lib/postgresql/data/pgdata");
    /// assert_eq!(
    ///     supervisor.staging_data_dir(),
    ///     Path::new("/var/lib/postgresql/data/pgdata_staging")
    /// );
    /// ```
    pub fn staging_data_dir(&self) -> PathBuf {
        if let Some(parent) = self.data_dir.parent() {
            if !parent.as_os_str().is_empty() {
                let name = self
                    .data_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("pgdata");
                return parent.join(format!("{name}_staging"));
            }
        }
        let name = self
            .data_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pgdata");
        PathBuf::from(format!("{name}_staging"))
    }

    /// Returns the backup directory path used during atomic swap (`<data_dir>_old`).
    ///
    /// Sibling directory located in the same parent directory as `data_dir`.
    ///
    /// # Example
    /// ```no_run
    /// use std::path::Path;
    /// use pgvisor_sidecar::supervisor::PostgresSupervisor;
    ///
    /// let supervisor = PostgresSupervisor::new("/var/lib/postgresql/data/pgdata");
    /// assert_eq!(
    ///     supervisor.old_data_dir(),
    ///     Path::new("/var/lib/postgresql/data/pgdata_old")
    /// );
    /// ```
    pub fn old_data_dir(&self) -> PathBuf {
        if let Some(parent) = self.data_dir.parent() {
            if !parent.as_os_str().is_empty() {
                let name = self
                    .data_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("pgdata");
                return parent.join(format!("{name}_old"));
            }
        }
        let name = self
            .data_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pgdata");
        PathBuf::from(format!("{name}_old"))
    }

    /// Generates the configuration for the shadow PostgreSQL instance running in staging.
    ///
    /// Sets port to `config.port + 1` (default 5433), removes standby conninfo (restored node is leader),
    /// and configures PITR target and restore_command if requested.
    ///
    /// # Arguments
    /// * `config` - Base PostgreSQL configuration of the running instance.
    /// * `recovery_target_time` - Optional PITR recovery target timestamp string.
    ///
    /// # Returns
    /// Cloned and adjusted `PostgresConfig` for shadow process recovery.
    pub fn generate_staging_config(
        config: &PostgresConfig,
        recovery_target_time: Option<&str>,
    ) -> PostgresConfig {
        let mut staging_config = config.clone();
        staging_config.port = config.port.wrapping_add(1);
        staging_config.primary_conninfo = None;

        if let Some(target_time) = recovery_target_time {
            let sidecar_bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_else(|| "pgvisor-sidecar".to_string());

            if staging_config.restore_command.is_none() {
                staging_config.restore_command = Some(format!("{sidecar_bin} restore %f %p"));
            }
            staging_config.recovery_target_time = Some(target_time.to_string());
            staging_config.recovery_target_action = Some("promote".to_string());
        }

        staging_config
    }

    /// Prepares staging directory by clearing existing staging files and extracting basebackup snapshot tarball.
    ///
    /// Removes residual configuration and indicator files (`standby.signal`, `postgresql.auto.conf`, `backup_label.old`)
    /// and sets secure directory permissions (0700). Active production data directory remains untouched.
    ///
    /// # Arguments
    /// * `tar_bytes` - Gzipped tar archive bytes of the basebackup snapshot.
    ///
    /// # Returns
    /// Path to the extracted staging data directory.
    pub async fn prepare_staging_directory(&self, tar_bytes: &[u8]) -> Result<PathBuf, SupervisorError> {
        let staging_dir = self.staging_data_dir();
        info!(dir = ?staging_dir, "Preparing clean staging data directory for shadow restore");

        if staging_dir.exists() {
            let mut entries = tokio::fs::read_dir(&staging_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&path).await;
                } else {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        } else {
            tokio::fs::create_dir_all(&staging_dir).await?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&staging_dir, std::fs::Permissions::from_mode(0o700)).await;
        }

        let temp_tar = staging_dir.join("restore_snapshot.tar.gz");
        tokio::fs::write(&temp_tar, tar_bytes).await?;

        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&temp_tar)
            .arg("-C")
            .arg(&staging_dir)
            .status()
            .await?;

        let _ = tokio::fs::remove_file(&temp_tar).await;

        if !status.success() {
            warn!("tar extraction into staging directory returned non-zero status");
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            return Err(SupervisorError::CommandFailed(
                "tar extraction returned non-zero status".to_string(),
            ));
        }

        // Clean up any residual standby signal or auto configuration from snapshot in staging
        let standby_signal = staging_dir.join("standby.signal");
        if standby_signal.exists() {
            let _ = tokio::fs::remove_file(&standby_signal).await;
        }
        let auto_conf = staging_dir.join("postgresql.auto.conf");
        if auto_conf.exists() {
            let _ = tokio::fs::remove_file(&auto_conf).await;
        }
        let old_label = staging_dir.join("backup_label.old");
        if old_label.exists() {
            let _ = tokio::fs::remove_file(&old_label).await;
        }

        info!(dir = ?staging_dir, "Staging directory prepared and snapshot extracted successfully");
        Ok(staging_dir)
    }

    /// Completely removes the staging data directory if present.
    ///
    /// Used during rollback or cleanup after restore completion.
    pub async fn clean_staging_directory(&self) {
        let staging_dir = self.staging_data_dir();
        if staging_dir.exists() {
            info!(dir = ?staging_dir, "Cleaning up staging data directory");
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        }
    }

    /// Spawns a shadow PostgreSQL process on staging data directory and alternate port (e.g. 5433).
    ///
    /// The shadow process executes in the background and is returned as a `tokio::process::Child`.
    /// The production PostgreSQL process (port 5432) remains running and supervised.
    ///
    /// # Arguments
    /// * `staging_dir` - Path to the extracted staging data directory.
    /// * `staging_config` - Configuration for shadow postgres (e.g. port 5433, recovery settings).
    ///
    /// # Returns
    /// The running `Child` process handle for the shadow PostgreSQL instance.
    pub async fn spawn_shadow_postgres(
        &self,
        staging_dir: &Path,
        staging_config: &PostgresConfig,
    ) -> Result<Child, SupervisorError> {
        info!(dir = ?staging_dir, port = staging_config.port, "Writing staging configurations");
        ConfigGenerator::write_configs(staging_dir, staging_config)?;

        info!(dir = ?staging_dir, port = staging_config.port, "Spawning shadow postgres process");
        let mut cmd = Command::new("postgres");
        cmd.arg("-D")
            .arg(staging_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        info!(pid, port = staging_config.port, "Shadow postgres process spawned");

        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    info!(target: "postgres_shadow", "{}", line);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    warn!(target: "postgres_shadow", "{}", line);
                }
            });
        }

        Ok(child)
    }

    /// Restores PostgreSQL data directory from basebackup tarball bytes.
    /// Gracefully shuts down Postgres if running, cleans data directory, extracts snapshot,
    /// writes recovery settings if requested, and restarts Postgres.
    pub async fn restore_from_snapshot(
        &self,
        tar_bytes: &[u8],
        config: &PostgresConfig,
        recovery_target_time: Option<&str>,
    ) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, "Initiating in-place cluster restore from snapshot");

        // 1. Mark status as Restoring before stopping Postgres
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Restoring;
        }
        let _ = self.stop().await;

        // 2. Clear old data directory
        if self.data_dir.exists() {
            let mut entries = tokio::fs::read_dir(&self.data_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&path).await;
                } else {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        } else {
            tokio::fs::create_dir_all(&self.data_dir).await?;
        }

        // 3. Extract tarball
        let temp_tar = self.data_dir.join("restore_snapshot.tar.gz");
        tokio::fs::write(&temp_tar, tar_bytes).await?;

        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&temp_tar)
            .arg("-C")
            .arg(&self.data_dir)
            .status()
            .await;

        let _ = tokio::fs::remove_file(&temp_tar).await;

        if let Ok(s) = status {
            if !s.success() {
                warn!("tar extraction returned non-zero status");
                return Err(SupervisorError::CommandFailed(
                    "tar extraction returned non-zero status".to_string(),
                ));
            }
        }

        // 4. Configure restore settings including PITR target and restore_command
        let mut restore_config = config.clone();
        // Restored node is always the cluster primary leader, never a standby replica
        restore_config.primary_conninfo = None;

        // Clean up any residual standby signal or auto configuration from snapshot
        let standby_signal = self.data_dir.join("standby.signal");
        if standby_signal.exists() {
            let _ = tokio::fs::remove_file(&standby_signal).await;
        }
        let auto_conf = self.data_dir.join("postgresql.auto.conf");
        if auto_conf.exists() {
            let _ = tokio::fs::remove_file(&auto_conf).await;
        }
        let old_label = self.data_dir.join("backup_label.old");
        if old_label.exists() {
            let _ = tokio::fs::remove_file(&old_label).await;
        }

        if let Some(target_time) = recovery_target_time {
            let sidecar_bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_else(|| "pgvisor-sidecar".to_string());

            if restore_config.restore_command.is_none() {
                restore_config.restore_command = Some(format!("{sidecar_bin} restore %f %p"));
            }
            restore_config.recovery_target_time = Some(target_time.to_string());
            restore_config.recovery_target_action = Some("promote".to_string());
        }

        // 5. Re-generate configs and restart Postgres
        self.start(&restore_config).await?;
        if self.child_pid() > 0 {
            if let Err(e) = self.wait_ready(config.port, 30).await {
                warn!(
                    ?e,
                    "PostgreSQL failed to become ready after restoring snapshot"
                );
                let mut st = self.status.lock().await;
                *st = ProcessStatus::Stopped;

                // Stop or kill any remnant child process
                let mut active = self.active_child.lock().await;
                if let Some(mut child) = active.take() {
                    let _ = child.kill().await;
                }
                self.child_pid.store(0, Ordering::SeqCst);

                // Clean up recovery.signal and reset recovery_target_* configs so
                // that an invalid target time does not leave the database permanently
                // broken or unable to start.
                let recovery_signal = self.data_dir.join("recovery.signal");
                if recovery_signal.exists() {
                    let _ = tokio::fs::remove_file(&recovery_signal).await;
                    info!("Cleaned up recovery.signal after failed restore attempt");
                }
                let mut clean_config = config.clone();
                clean_config.primary_conninfo = None;
                clean_config.recovery_target_time = None;
                clean_config.recovery_target_action = None;
                let _ = ConfigGenerator::write_conf_only(&self.data_dir, &clean_config);

                return Err(e);
            }
        }
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Running;
        }

        // 6. If targeted recovery was performed, clear recovery target settings now that Postgres is promoted
        if recovery_target_time.is_some() {
            let mut post_restore_config = config.clone();
            post_restore_config.primary_conninfo = None;
            post_restore_config.recovery_target_time = None;
            post_restore_config.recovery_target_action = None;
            // Use write_conf_only: Postgres's startup process is still running and owns
            // recovery.signal removal as part of its promote sequence. Touching it here
            // would race Postgres and cause a FATAL "could not remove file" crash.
            let _ = ConfigGenerator::write_conf_only(&self.data_dir, &post_restore_config);
        }

        info!("PostgreSQL restore from snapshot completed");
        Ok(())
    }

    /// Re-syncs standby replica from primary using pg_basebackup.
    pub async fn resync_from_primary(
        &self,
        primary_conninfo: &str,
        config: &PostgresConfig,
    ) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, primary_conninfo, "Re-syncing standby replica from primary");

        // 1. Mark status as Restoring before stopping Postgres
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Restoring;
        }
        let _ = self.stop().await;

        let res = self
            .resync_from_primary_inner(primary_conninfo, config)
            .await;
        if res.is_err() {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Fenced;
        }
        res
    }

    async fn resync_from_primary_inner(
        &self,
        primary_conninfo: &str,
        config: &PostgresConfig,
    ) -> Result<(), SupervisorError> {
        // 2. Wait for primary to accept replication connections BEFORE wiping local data
        let mut retries = 15;
        while retries > 0 {
            let status = Command::new("pg_isready")
                .arg("-d")
                .arg(primary_conninfo)
                .status()
                .await;
            if let Ok(s) = status {
                if s.success() {
                    info!("Primary is ready, proceeding with pg_basebackup re-sync");
                    break;
                }
            }
            retries -= 1;
            if retries == 0 {
                return Err(SupervisorError::CommandFailed(
                    "Primary did not become ready for replication re-sync".into(),
                ));
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }

        // 3. Clear old data directory now that primary readiness is confirmed
        if self.data_dir.exists() {
            let mut entries = tokio::fs::read_dir(&self.data_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&path).await;
                } else {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        } else {
            tokio::fs::create_dir_all(&self.data_dir).await?;
        }

        // 4. Clone fresh baseline from primary
        let status = Command::new("pg_basebackup")
            .arg("-d")
            .arg(primary_conninfo)
            .arg("-D")
            .arg(&self.data_dir)
            .arg("-Fp")
            .arg("-Xs")
            .arg("-R")
            .status()
            .await?;

        if !status.success() {
            return Err(SupervisorError::CommandFailed(format!(
                "pg_basebackup re-sync failed with status: {status}"
            )));
        }

        // 5. Start Postgres with standby configuration
        let mut standby_config = config.clone();
        standby_config.primary_conninfo = Some(primary_conninfo.to_string());
        self.start(&standby_config).await?;
        if self.child_pid() > 0 {
            self.wait_ready(config.port, 30).await?;
        }
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Running;
        }

        info!("Standby replica successfully re-synced and ready");
        Ok(())
    }

    /// Polls pg_isready until Postgres is accepting connections or timeout occurs.
    pub async fn wait_ready(&self, port: u16, max_retries: u32) -> Result<(), SupervisorError> {
        let mut retries = max_retries;
        while retries > 0 {
            // Check if child process exited unexpectedly to fail fast
            {
                let mut active = self.active_child.lock().await;
                if let Some(child) = active.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        self.child_pid.store(0, Ordering::SeqCst);
                        let mut st = self.status.lock().await;
                        if *st != ProcessStatus::Fenced {
                            *st = ProcessStatus::Stopped;
                        }
                        return Err(SupervisorError::CommandFailed(format!(
                            "Postgres process exited unexpectedly with status: {status}"
                        )));
                    }
                }
            }

            let status = Command::new("pg_isready")
                .arg("-h")
                .arg("127.0.0.1")
                .arg("-p")
                .arg(port.to_string())
                .arg("-U")
                .arg("postgres")
                .status()
                .await;
            if let Ok(s) = status {
                if s.success() {
                    return Ok(());
                }
            }
            retries -= 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        Err(SupervisorError::CommandFailed(
            "Postgres did not become ready within timeout".into(),
        ))
    }

    /// Appends a log line to the in-memory circular buffer.
    pub async fn append_log(&self, level: LogLevel, message: impl Into<String>) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let Ok(mut buf) = self.logs.write() else {
            return;
        };
        if buf.len() >= MAX_LOG_ENTRIES {
            buf.pop_front();
        }
        buf.push_back(NodeLogEntry {
            timestamp_ms: now,
            level,
            message: message.into(),
        });
    }

    /// Retrieves up to `limit` recent buffered log entries.
    pub async fn recent_logs(&self, limit: usize) -> Vec<NodeLogEntry> {
        let Ok(buf) = self.logs.read() else {
            return Vec::new();
        };
        let total = buf.len();
        let count = limit.min(total);
        let start_idx = total.saturating_sub(count);
        buf.iter().skip(start_idx).cloned().collect()
    }

    /// Total number of currently buffered log entries.
    pub async fn total_buffered_logs(&self) -> usize {
        self.logs.read().map(|b| b.len()).unwrap_or(0)
    }

    /// Reads a diagnostic or configuration file from the PostgreSQL data directory.
    pub async fn read_node_file(&self, config_type: NodeConfigType) -> Result<NodeConfigResponse, SupervisorError> {
        let filename = config_type.filename();
        let path = self.data_dir.join(filename);
        let path_str = path.to_string_lossy().to_string();

        if !path.exists() {
            return Ok(NodeConfigResponse {
                node_id: 0,
                file_type: config_type,
                filename: filename.to_string(),
                path: path_str,
                exists: false,
                content: String::new(),
                size_bytes: 0,
                modified_at_ms: None,
            });
        }

        let metadata = tokio::fs::metadata(&path).await?;
        let size_bytes = metadata.len();
        let modified_at_ms = metadata.modified().ok().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
        });

        // Limit read size to 1MB to prevent excessive memory consumption
        let content = if size_bytes > 1024 * 1024 {
            let mut file = tokio::fs::File::open(&path).await?;
            let mut buffer = vec![0u8; 1024 * 1024];
            let n = tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await?;
            let mut s = String::from_utf8_lossy(&buffer[..n]).to_string();
            s.push_str("\n... [truncated: file exceeds 1MB] ...");
            s
        } else {
            match tokio::fs::read(&path).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(e) => format!("Error reading {}: {}", filename, e),
            }
        };

        Ok(NodeConfigResponse {
            node_id: 0,
            file_type: config_type,
            filename: filename.to_string(),
            path: path_str,
            exists: true,
            content,
            size_bytes,
            modified_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_supervisor_initial_state() {
        let dir = tempdir().unwrap();
        let supervisor = PostgresSupervisor::new(dir.path());
        assert_eq!(supervisor.status().await, ProcessStatus::Stopped);
        assert_eq!(supervisor.child_pid(), 0);
    }

    #[tokio::test]
    async fn test_restore_from_snapshot_invalid_tar() {
        let dir = tempdir().unwrap();
        let supervisor = PostgresSupervisor::new(dir.path());
        let config = PostgresConfig {
            port: 59999,
            ..Default::default()
        };

        let dummy_tar = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
        let res = supervisor
            .restore_from_snapshot(&dummy_tar, &config, Some("2026-09-05 05:00:00 UTC"))
            .await;

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_supervisor_log_buffering() {
        let dir = tempdir().unwrap();
        let supervisor = PostgresSupervisor::new(dir.path());
        assert_eq!(supervisor.total_buffered_logs().await, 0);

        supervisor.append_log(LogLevel::Info, "test log line 1").await;
        supervisor.append_log(LogLevel::Warn, "test log line 2").await;
        assert_eq!(supervisor.total_buffered_logs().await, 2);

        let logs = supervisor.recent_logs(10).await;
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "test log line 1");
        assert_eq!(logs[0].level, LogLevel::Info);
        assert_eq!(logs[1].message, "test log line 2");
        assert_eq!(logs[1].level, LogLevel::Warn);

        let limited = supervisor.recent_logs(1).await;
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].message, "test log line 2");
    }

    #[tokio::test]
    async fn test_supervisor_read_node_file() {
        let dir = tempdir().unwrap();
        let supervisor = PostgresSupervisor::new(dir.path());

        // File does not exist
        let resp = supervisor.read_node_file(NodeConfigType::PostgresqlConf).await.unwrap();
        assert!(!resp.exists);
        assert_eq!(resp.content, "");
        assert_eq!(resp.path, dir.path().join("postgresql.conf").to_string_lossy().to_string());

        // Create file
        let conf_path = dir.path().join("postgresql.conf");
        tokio::fs::write(&conf_path, "port = 5432\nshared_buffers = 128MB\n").await.unwrap();

        let resp2 = supervisor.read_node_file(NodeConfigType::PostgresqlConf).await.unwrap();
        assert!(resp2.exists);
        assert_eq!(resp2.filename, "postgresql.conf");
        assert_eq!(resp2.path, conf_path.to_string_lossy().to_string());
        assert!(resp2.content.contains("port = 5432"));
        assert_eq!(resp2.file_type, NodeConfigType::PostgresqlConf);
    }

    #[test]
    fn test_staging_and_old_data_dir_paths() {
        let supervisor = PostgresSupervisor::new("/var/lib/postgresql/data/pgdata");
        assert_eq!(
            supervisor.staging_data_dir(),
            PathBuf::from("/var/lib/postgresql/data/pgdata_staging")
        );
        assert_eq!(
            supervisor.old_data_dir(),
            PathBuf::from("/var/lib/postgresql/data/pgdata_old")
        );

        let relative_supervisor = PostgresSupervisor::new("pgdata");
        assert_eq!(
            relative_supervisor.staging_data_dir(),
            PathBuf::from("pgdata_staging")
        );
        assert_eq!(
            relative_supervisor.old_data_dir(),
            PathBuf::from("pgdata_old")
        );
    }

    #[test]
    fn test_generate_staging_config() {
        let base_config = PostgresConfig {
            port: 5432,
            primary_conninfo: Some("host=node1 port=5432 user=replicator".into()),
            recovery_target_time: None,
            recovery_target_action: None,
            ..Default::default()
        };

        let staging_config = PostgresSupervisor::generate_staging_config(
            &base_config,
            Some("2026-11-12 06:45:00 UTC"),
        );

        assert_eq!(staging_config.port, 5433);
        assert!(staging_config.primary_conninfo.is_none());
        assert_eq!(
            staging_config.recovery_target_time.as_deref(),
            Some("2026-11-12 06:45:00 UTC")
        );
        assert_eq!(
            staging_config.recovery_target_action.as_deref(),
            Some("promote")
        );
        assert!(staging_config.restore_command.is_some());
        assert!(staging_config
            .restore_command
            .as_ref()
            .unwrap()
            .contains("restore %f %p"));
    }

    #[tokio::test]
    async fn test_prepare_staging_directory_invalid_tar() {
        let dir = tempdir().unwrap();
        let prod_data = dir.path().join("pgdata");
        tokio::fs::create_dir_all(&prod_data).await.unwrap();
        tokio::fs::write(prod_data.join("PG_VERSION"), "16\n").await.unwrap();

        let supervisor = PostgresSupervisor::new(&prod_data);
        let dummy_corrupt_tar = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];

        let result = supervisor.prepare_staging_directory(&dummy_corrupt_tar).await;
        assert!(result.is_err());

        // Production data must remain completely intact
        assert!(prod_data.join("PG_VERSION").exists());
        let prod_content = tokio::fs::read_to_string(prod_data.join("PG_VERSION")).await.unwrap();
        assert_eq!(prod_content, "16\n");

        // Staging directory must be cleaned up on failure
        assert!(!supervisor.staging_data_dir().exists());
    }
}
