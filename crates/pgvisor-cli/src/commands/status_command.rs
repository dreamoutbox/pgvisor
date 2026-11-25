use std::path::Path;

use crate::docker::{CommandExecutor, ComposeRunner};
use crate::error::CliError;

/// Executes `pgvisor status` to display container states.
pub struct StatusCommand<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> StatusCommand<'a> {
    /// Creates a `StatusCommand` using the given `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Runs `docker compose ps` for `dir`.
    pub fn execute(&self, dir: &Path) -> Result<(), CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }
        compose.ps()
    }
}
