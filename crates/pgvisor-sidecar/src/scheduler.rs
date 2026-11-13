use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use croner::Cron;
use pgvisor_core::api::SidecarRole;
use pgvisor_core::backup::{BackupScheduleConfig, BackupType};
use tracing::{debug, error, info, warn};

use crate::api::backup::create_backup_internal;
use crate::api::{ApiError, ApiState};

pub const SIDECAR_BACKUP_CRON_ENV: &str = "PGVISOR_SIDECAR_BACKUP_CRON";
const SCHEDULER_ACTOR: &str = "pgvisor-scheduler";
const CRON_COMPUTE_RETRY_SECS: u64 = 60;

/// Returns whether the sidecar backup scheduler should actively spawn cron workers.
///
/// Defaults to disabled (`false`) until Phase 9 cutover removes the legacy proxy scheduler.
pub fn is_sidecar_cron_enabled(raw_env: Option<&str>, config_cron_enabled: bool) -> bool {
    if !config_cron_enabled {
        return false;
    }
    raw_env.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

/// Leader-gated background scheduler driving automated full and incremental backups on the sidecar.
pub struct BackupScheduler {
    api_state: ApiState,
    config: BackupScheduleConfig,
}

impl BackupScheduler {
    /// Creates a new [`BackupScheduler`] bound to `api_state` and `config`.
    pub fn new(api_state: ApiState, config: BackupScheduleConfig) -> Self {
        Self { api_state, config }
    }

    /// Spawns full and incremental backup CRON worker tasks when enabled.
    pub fn start(self: Arc<Self>) {
        let raw_flag = std::env::var(SIDECAR_BACKUP_CRON_ENV).ok();
        if !is_sidecar_cron_enabled(raw_flag.as_deref(), self.config.cron_enabled) {
            info!(
                env = SIDECAR_BACKUP_CRON_ENV,
                config_enabled = self.config.cron_enabled,
                "Sidecar automated backup CRON scheduling is disabled"
            );
            return;
        }

        let scheduler_full = Arc::clone(&self);
        tokio::spawn(async move {
            scheduler_full
                .run_job(BackupType::Full, &scheduler_full.config.full_backup_cron)
                .await;
        });

        let scheduler_incr = self;
        tokio::spawn(async move {
            scheduler_incr
                .run_job(
                    BackupType::Incremental,
                    &scheduler_incr.config.incremental_backup_cron,
                )
                .await;
        });
    }

    /// Executes a single scheduled tick if and only if this sidecar currently holds [`SidecarRole::Leader`].
    pub async fn run_tick_if_leader(&self, b_type: BackupType) {
        if self.api_state.sidecar.current_role().await != SidecarRole::Leader {
            debug!(?b_type, "Skipping scheduled backup tick: node is not cluster leader");
            return;
        }

        let prefix = match b_type {
            BackupType::Full => "cron-full",
            BackupType::Incremental => "cron-incr",
        };
        let label = format!("{prefix}-{}", Utc::now().format("%Y%m%d-%H%M%S"));
        info!(?b_type, %label, "Triggering leader-gated scheduled automated backup");

        match create_backup_internal(&self.api_state, b_type, Some(label), SCHEDULER_ACTOR).await {
            Ok(meta) => {
                info!(
                    ?b_type,
                    snapshot_id = %meta.snapshot_id,
                    "Scheduled automated backup completed successfully"
                );
            }
            Err(ApiError::Conflict(_)) => {
                warn!(
                    ?b_type,
                    "Scheduled automated backup skipped: another backup or restore is in progress"
                );
            }
            Err(err) => {
                error!(?b_type, ?err, "Scheduled automated backup failed");
            }
        }
    }

    async fn run_job(&self, b_type: BackupType, cron_expr: &str) {
        let cron = match Cron::from_str(cron_expr) {
            Ok(c) => c,
            Err(err) => {
                error!(
                    ?b_type,
                    cron_expr,
                    ?err,
                    "Failed to parse CRON expression; disabling schedule"
                );
                return;
            }
        };

        info!(
            ?b_type,
            cron_expr, "Started leader-gated sidecar backup CRON worker"
        );

        loop {
            let now = Utc::now();
            let next_run = match cron.find_next_occurrence(&now, false) {
                Ok(next) => next,
                Err(err) => {
                    error!(
                        ?b_type,
                        ?err,
                        "Failed to compute next CRON occurrence; retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(CRON_COMPUTE_RETRY_SECS)).await;
                    continue;
                }
            };

            let delay = (next_run - now).to_std().unwrap_or_else(|_| Duration::from_secs(1));
            info!(
                ?b_type,
                next_run = %next_run.format("%Y-%m-%d %H:%M:%S UTC"),
                delay_secs = delay.as_secs(),
                "Scheduled next automated sidecar backup"
            );
            tokio::time::sleep(delay).await;

            self.run_tick_if_leader(b_type).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidecar_cron_gate_and_parsing() {
        assert!(!is_sidecar_cron_enabled(None, true));
        assert!(!is_sidecar_cron_enabled(Some("false"), true));
        assert!(!is_sidecar_cron_enabled(Some("true"), false));
        assert!(is_sidecar_cron_enabled(Some("true"), true));
        assert!(is_sidecar_cron_enabled(Some("1"), true));

        assert!(Cron::from_str("0 1 * * *").is_ok());
        assert!(Cron::from_str("0 * * * *").is_ok());
        assert!(Cron::from_str("not-a-cron").is_err());
    }
}
