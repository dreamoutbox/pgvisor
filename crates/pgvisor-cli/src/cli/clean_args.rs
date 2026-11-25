use clap::Args;

/// CLI arguments for `pgvisor clean`.
#[derive(Debug, Clone, Args)]
pub struct CleanArgs {
    /// Skip interactive confirmation prompt before deleting database volumes.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}
