use std::path::Path;
use std::time::Duration;

use crate::cli::RestartArgs;
use crate::config::ClusterServices;
use crate::docker::{CommandExecutor, ComposeRunner, HealthWaiter};
use crate::error::CliError;

/// Executes `pgvisor restart` and waits for required services to report healthy.
pub struct RestartCommand<'a> {
    executor: &'a dyn CommandExecutor,
    poll_interval: Duration,
}

impl<'a> RestartCommand<'a> {
    /// Creates a `RestartCommand` with a 1-second health poll interval.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self {
            executor,
            poll_interval: Duration::from_secs(1),
        }
    }

    /// Overrides the healthcheck poll interval (primarily for deterministic tests).
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Restarts cluster containers in `dir` and polls healthchecks until `args.timeout`.
    pub fn execute(&self, dir: &Path, args: &RestartArgs, silent: bool) -> Result<(), CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }
        if !silent {
            println!("Restarting PgVisor cluster containers...");
        }
        compose.restart()?;

        let required_services = ClusterServices::required_for_compose(compose.compose_file())?;
        let waiter = HealthWaiter::new(
            self.executor,
            self.poll_interval,
            Duration::from_secs(args.timeout),
        );
        waiter.wait_for_services(&compose, &required_services)?;
        if !silent {
            println!("PgVisor cluster restarted and healthy.");
        }
        Ok(())
    }
}
