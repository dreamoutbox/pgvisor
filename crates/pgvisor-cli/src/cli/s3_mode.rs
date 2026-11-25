use clap::ValueEnum;

/// Object storage deployment mode for `pgvisor setup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum S3Mode {
    /// Provision a bundled RustFS S3-compatible container alongside the cluster.
    Bundled,
    /// Connect the cluster to an existing external S3-compatible endpoint.
    External,
}
