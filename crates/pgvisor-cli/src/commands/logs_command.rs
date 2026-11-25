use std::path::Path;

use crate::cli::LogsArgs;
use crate::docker::{CommandExecutor, ComposeRunner};
use crate::error::CliError;

/// Executes `pgvisor logs` to stream or print cluster container logs.
pub struct LogsCommand<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> LogsCommand<'a> {
    /// Creates a `LogsCommand` using the given `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Runs `docker compose logs` with follow, tail, and service filters.
    pub fn execute(&self, dir: &Path, args: &LogsArgs) -> Result<(), CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }
        compose.logs(!args.no_follow, args.tail, &args.services)
    }
}
