use clap::Args;

/// CLI arguments for `pgvisor logs`.
#[derive(Debug, Clone, Args)]
pub struct LogsArgs {
    /// Print existing logs and exit instead of following (`-f`).
    #[arg(long)]
    pub no_follow: bool,

    /// Number of lines to show from the end of the logs.
    #[arg(long)]
    pub tail: Option<u32>,

    /// Optional service names to filter log output.
    #[arg(trailing_var_arg = true)]
    pub services: Vec<String>,
}
