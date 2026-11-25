use crate::docker::command_output::CommandOutput;
use crate::error::CliError;

/// Abstraction over external process execution for deterministic testing.
pub trait CommandExecutor {
    /// Executes `program` with `args`, capturing stdout and stderr.
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CliError>;

    /// Executes `program` with `args`, inheriting stdin/stdout/stderr for interactive streaming.
    fn run_interactive(&self, program: &str, args: &[&str]) -> Result<i32, CliError>;

    /// Returns `true` if `program` is discoverable on `PATH`.
    fn is_in_path(&self, program: &str) -> bool;
}
