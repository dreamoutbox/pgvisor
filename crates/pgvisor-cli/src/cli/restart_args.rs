use clap::Args;

/// CLI arguments for `pgvisor restart`.
#[derive(Debug, Clone, Args)]
pub struct RestartArgs {
    /// Maximum seconds to wait for required services to report healthy after restart.
    #[arg(long, default_value_t = 90)]
    pub timeout: u64,
}
