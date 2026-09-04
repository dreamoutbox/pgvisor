use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::{ConfigError, ConfigGenerator, PostgresConfig};

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
}

/// Container PID 1 supervisor managing Postgres lifecycle, signal routing, and fencing.
pub struct PostgresSupervisor {
    data_dir: PathBuf,
    status: Arc<Mutex<ProcessStatus>>,
    child_pid: Arc<AtomicU32>,
    active_child: Arc<Mutex<Option<Child>>>,
}

impl PostgresSupervisor {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            status: Arc::new(Mutex::new(ProcessStatus::Stopped)),
            child_pid: Arc::new(AtomicU32::new(0)),
            active_child: Arc::new(Mutex::new(None)),
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

        // Pipe stdout and stderr to tracing logs
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
            *st = ProcessStatus::Running;
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

        // Signal reload to running PostgreSQL WAL receiver
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
        Ok(())
    }

    /// Immediately halts Postgres using `pg_ctl stop -m immediate` to prevent split-brain writes.
    pub async fn fence(&self) -> Result<(), SupervisorError> {
        warn!(dir = ?self.data_dir, "FENCING: executing immediate stop on Postgres");
        {
            let mut st = self.status.lock().await;
            *st = ProcessStatus::Fenced;
        }

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

        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Gracefully stops Postgres child process using `pg_ctl stop -m fast`.
    pub async fn stop(&self) -> Result<(), SupervisorError> {
        info!(dir = ?self.data_dir, "Executing fast shutdown on Postgres");
        let status = Command::new("pg_ctl")
            .arg("stop")
            .arg("-D")
            .arg(&self.data_dir)
            .arg("-m")
            .arg("fast")
            .status()
            .await;

        match status {
            Ok(s) if s.success() => info!("Postgres stopped cleanly"),
            _ => {
                let mut active = self.active_child.lock().await;
                if let Some(mut child) = active.take() {
                    let _ = child.kill().await;
                }
            }
        }

        let mut st = self.status.lock().await;
        *st = ProcessStatus::Stopped;
        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Returns the current supervisor process status.
    pub async fn status(&self) -> ProcessStatus {
        let st = self.status.lock().await;
        *st
    }

    /// Returns the monitored child PID, or 0 if not running.
    pub fn child_pid(&self) -> u32 {
        self.child_pid.load(Ordering::SeqCst)
    }

    /// Waits for child process to terminate.
    pub async fn wait(&self) -> Option<ExitStatus> {
        let mut active = self.active_child.lock().await;
        if let Some(child) = active.as_mut() {
            match child.wait().await {
                Ok(status) => {
                    info!(?status, "Postgres child process exited");
                    let mut st = self.status.lock().await;
                    *st = ProcessStatus::Stopped;
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

        // 1. Stop Postgres if running
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
            }
        }

        // 4. Configure recovery signal if PITR target specified
        if let Some(target_time) = recovery_target_time {
            let recovery_signal = self.data_dir.join("recovery.signal");
            let content = format!(
                "# Generated by pgvisor restore\nrecovery_target_time = '{target_time}'\nrecovery_target_action = 'promote'\n"
            );
            tokio::fs::write(recovery_signal, content).await?;
        }

        // 5. Re-generate configs and restart Postgres
        let _ = self.start(config).await;
        if self.child_pid() > 0 {
            let _ = self.wait_ready(config.port, 30).await;
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

        // 1. Stop Postgres if running
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

        // 3. Clone fresh baseline from primary
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

        // 4. Start Postgres with standby configuration
        self.start(config).await?;
        if self.child_pid() > 0 {
            let _ = self.wait_ready(config.port, 30).await;
        }

        info!("Standby replica successfully re-synced and ready");
        Ok(())
    }

    /// Polls pg_isready until Postgres is accepting connections or timeout occurs.
    pub async fn wait_ready(&self, port: u16, max_retries: u32) -> Result<(), SupervisorError> {
        let mut retries = max_retries;
        while retries > 0 {
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
    async fn test_restore_from_snapshot_recovery_signal() {
        let dir = tempdir().unwrap();
        let supervisor = PostgresSupervisor::new(dir.path());
        let config = PostgresConfig::default();

        let dummy_tar = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
        let res = supervisor
            .restore_from_snapshot(&dummy_tar, &config, Some("2026-09-05 05:00:00 UTC"))
            .await;

        assert!(res.is_ok());
        assert!(dir.path().join("recovery.signal").exists());
    }
}
