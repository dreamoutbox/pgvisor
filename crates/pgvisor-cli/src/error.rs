use std::path::PathBuf;

use thiserror::Error;

/// Structured error type for `pgvisor-cli` operations.
#[derive(Debug, Error)]
pub enum CliError {
    /// Docker CLI or Compose v2+ preflight check failed.
    #[error("Docker preflight check failed: {0}")]
    PreflightFailed(String),

    /// Required TCP port is already in use on the target bind address.
    #[error("TCP port {port} on {bind_addr} is already in use: {reason}")]
    PortInUse {
        /// Bind IP address checked.
        bind_addr: String,
        /// Port number checked.
        port: u16,
        /// Underlying bind error message.
        reason: String,
    },

    /// Filesystem I/O failure.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// External command failed to spawn or exited non-zero.
    #[error("Command `{command}` failed (exit {status_code}): {stderr}")]
    CommandFailed {
        /// Rendered command invocation.
        command: String,
        /// Process exit code (or -1 if unavailable).
        status_code: i32,
        /// Captured standard error output.
        stderr: String,
    },

    /// Target cluster directory has not been initialized with `pgvisor setup`.
    #[error("Cluster compose file not found at {0}. Run `pgvisor setup` first.")]
    MissingComposeFile(PathBuf),

    /// Setup refused to overwrite an existing configuration file without `--force`.
    #[error("File already exists at {0}. Pass `--force` to overwrite.")]
    FileAlreadyExists(PathBuf),

    /// `.env` file syntax error.
    #[error("Invalid .env line {line_number} in {path}: {reason}")]
    InvalidEnvFile {
        /// Path to the `.env` file.
        path: PathBuf,
        /// 1-based line number.
        line_number: usize,
        /// Reason parsing failed.
        reason: String,
    },

    /// Invalid CLI flag or option combination.
    #[error("Invalid option: {0}")]
    InvalidOption(String),

    /// Required container exited or died during startup/restart.
    #[error("Container '{name}' entered state '{state}'. Recent logs:\n{logs}")]
    ContainerExited {
        /// Container name or ID.
        name: String,
        /// Observed container state.
        state: String,
        /// Last 30 lines of container logs.
        logs: String,
    },

    /// Healthcheck polling timed out before all required services became healthy.
    #[error("Timed out after {timeout_secs}s waiting for healthy services:\n{details}")]
    HealthTimeout {
        /// Configured timeout in seconds.
        timeout_secs: u64,
        /// Per-service status summary.
        details: String,
    },

    /// Stubbed subcommand not yet implemented in the current phase.
    #[error("Subcommand '{0}' is not implemented yet")]
    NotImplemented(&'static str),
}
