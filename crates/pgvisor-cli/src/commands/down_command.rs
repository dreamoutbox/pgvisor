use std::path::Path;

use crate::docker::{CommandExecutor, ComposeRunner};
use crate::error::CliError;

/// Executes `pgvisor down` (and `stop` alias) to stop cluster containers while preserving volumes.
pub struct DownCommand<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> DownCommand<'a> {
    /// Creates a `DownCommand` using the given `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Stops the cluster containers in `dir`.
    pub fn execute(&self, dir: &Path, silent: bool) -> Result<(), CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }
        if !silent {
            println!("Stopping PgVisor cluster containers (database volumes preserved)...");
        }
        compose.down()?;
        if !silent {
            println!("PgVisor cluster stopped.");
        }
        Ok(())
    }
}
