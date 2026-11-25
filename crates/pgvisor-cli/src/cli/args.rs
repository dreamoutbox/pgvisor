use std::path::PathBuf;

use clap::Parser;

use crate::cli::command::CliCommand;

/// Top-level CLI arguments for `pgvisor`.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "pgvisor",
    version,
    about = "PgVisor PostgreSQL HA cluster bootstrap and lifecycle CLI"
)]
pub struct CliArgs {
    /// Target cluster directory containing `docker-compose.yml` and `.env`.
    #[arg(short = 'C', long = "dir", global = true, default_value = ".")]
    pub dir: PathBuf,

    /// Suppress non-essential status output.
    #[arg(short = 's', long = "silent", global = true)]
    pub silent: bool,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: CliCommand,
}
