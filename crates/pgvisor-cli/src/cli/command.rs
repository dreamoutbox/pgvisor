use clap::Subcommand;

use crate::cli::clean_args::CleanArgs;
use crate::cli::logs_args::LogsArgs;
use crate::cli::restart_args::RestartArgs;
use crate::cli::setup_args::SetupArgs;
use crate::cli::up_args::UpArgs;

/// Subcommands supported by the `pgvisor` CLI.
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Provision standalone docker-compose.yml, .env, and random cluster secrets.
    Setup(SetupArgs),

    /// Start the PgVisor HA cluster and wait for all required services to become healthy.
    Up(UpArgs),

    /// Stop cluster containers while preserving database volumes.
    #[command(alias = "stop")]
    Down,

    /// Restart cluster containers and wait for healthchecks.
    Restart(RestartArgs),

    /// Show running containers and cluster status.
    Status,

    /// Tail or print cluster container logs.
    Logs(LogsArgs),

    /// Stop the cluster and delete all persistent database volumes.
    Clean(CleanArgs),
}
