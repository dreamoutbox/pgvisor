use clap::Args;

/// CLI arguments for `pgvisor up`.
#[derive(Debug, Clone, Args)]
pub struct UpArgs {
    /// Rebuild Docker images from local source before starting (requires local Dockerfile).
    #[arg(long)]
    pub build: bool,

    /// Maximum seconds to wait for required services to report healthy.
    #[arg(long, default_value_t = 90)]
    pub timeout: u64,
}
