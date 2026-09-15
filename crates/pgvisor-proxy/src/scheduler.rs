use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use croner::Cron;
use pgvisor_core::backup::{BackupScheduleConfig, BackupType};
use pgvisor_dashboard::handlers::BackupService;
use tracing::{error, info, warn};

/// Background scheduler driving automated full and incremental backups based on CRON expressions.
pub struct BackupScheduler {
    backup_service: Arc<dyn BackupService>,
    config: BackupScheduleConfig,
}

impl BackupScheduler {
    pub fn new(backup_service: Arc<dyn BackupService>, config: BackupScheduleConfig) -> Self {
        Self {
            backup_service,
            config,
        }
    }

    /// Spawns the scheduler loop tasks for full and incremental backups.
    pub fn start(self: Arc<Self>) {
        if !self.config.cron_enabled {
            info!("Automated backup CRON scheduling is disabled");
            return;
        }

        let scheduler_full = self.clone();
        tokio::spawn(async move {
            scheduler_full
                .run_job(BackupType::Full, &scheduler_full.config.full_backup_cron)
                .await;
        });

        let scheduler_incr = self.clone();
        tokio::spawn(async move {
            scheduler_incr
                .run_job(
                    BackupType::Incremental,
                    &scheduler_incr.config.incremental_backup_cron,
                )
                .await;
        });
    }

    async fn run_job(&self, b_type: BackupType, cron_expr: &str) {
        let cron = match Cron::from_str(cron_expr) {
            Ok(c) => c,
            Err(e) => {
                error!(
                    ?b_type,
                    cron_expr,
                    ?e,
                    "Failed to parse CRON expression; disabling schedule"
                );
                return;
            }
        };

        info!(
            ?b_type,
            cron_expr, "Started automated backup CRON schedule worker"
        );

        loop {
            let now = Utc::now();
            let next_run = match cron.find_next_occurrence(&now, false) {
                Ok(next) => next,
                Err(e) => {
                    error!(
                        ?b_type,
                        ?e,
                        "Failed to compute next CRON occurrence; retrying in 60s"
                    );
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
            };

            let delay = match (next_run - now).to_std() {
                Ok(d) => d,
                Err(_) => Duration::from_secs(1),
            };

            info!(
                ?b_type,
                next_run = %next_run.format("%Y-%m-%d %H:%M:%S UTC"),
                delay_secs = delay.as_secs(),
                "Scheduled next automated backup"
            );
            tokio::time::sleep(delay).await;

            let label = match b_type {
                BackupType::Full => format!("cron-full-{}", Utc::now().format("%Y%m%d-%H%M%S")),
                BackupType::Incremental => {
                    format!("cron-incr-{}", Utc::now().format("%Y%m%d-%H%M%S"))
                }
            };

            info!(?b_type, %label, "Triggering scheduled automated backup");
            match self.backup_service.create_backup(b_type, Some(label)).await {
                Ok(meta) => {
                    info!(
                        ?b_type,
                        snapshot_id = %meta.snapshot_id,
                        "Scheduled automated backup completed successfully"
                    );
                }
                Err(err) => {
                    if err.contains("already in progress") {
                        warn!(
                            ?b_type,
                            "Scheduled automated backup skipped: another operation is in progress"
                        );
                    } else {
                        error!(?b_type, ?err, "Scheduled automated backup failed");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_expression_parsing() {
        let full_cron = Cron::from_str("0 1 * * *");
        assert!(full_cron.is_ok());

        let incr_cron = Cron::from_str("0 * * * *");
        assert!(incr_cron.is_ok());

        let invalid_cron = Cron::from_str("not-a-cron");
        assert!(invalid_cron.is_err());

        let now = Utc::now();
        let next = full_cron.unwrap().find_next_occurrence(&now, false);
        assert!(next.is_ok());
        assert!(next.unwrap() > now);
    }
}
