use clap::Args;

use crate::cli::s3_mode::S3Mode;

/// CLI arguments for `pgvisor setup`.
#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    /// Skip interactive prompts and accept defaults/flags.
    #[arg(short = 'y', long = "yes", visible_alias = "non-interactive")]
    pub non_interactive: bool,

    /// Cluster identifier written to `PGVISOR_CLUSTER_ID`.
    #[arg(long)]
    pub cluster_id: Option<String>,

    /// PgVisor container image to deploy.
    #[arg(long)]
    pub image: Option<String>,

    /// Host port exposed for the PostgreSQL L7 proxy.
    #[arg(long)]
    pub proxy_port: Option<u16>,

    /// Host bind address for the standalone web dashboard.
    #[arg(long, default_value = "127.0.0.1")]
    pub dashboard_bind: String,

    /// Host port exposed for the standalone web dashboard.
    #[arg(long)]
    pub dashboard_port: Option<u16>,

    /// Comma-separated PostgreSQL superuser roles allowed to log in to the dashboard/API.
    #[arg(long)]
    pub superuser: Option<String>,

    /// S3 storage mode (`bundled` RustFS or `external`).
    #[arg(long, value_enum)]
    pub s3_mode: Option<S3Mode>,

    /// Host port for bundled RustFS S3 API.
    #[arg(long)]
    pub s3_api_port: Option<u16>,

    /// Host port for bundled RustFS web console.
    #[arg(long)]
    pub s3_console_port: Option<u16>,

    /// External S3 endpoint URL (required when `--s3-mode external`).
    #[arg(long)]
    pub s3_endpoint: Option<String>,

    /// S3 bucket name for basebackups and WAL archives.
    #[arg(long)]
    pub s3_bucket: Option<String>,

    /// S3 region name.
    #[arg(long)]
    pub s3_region: Option<String>,

    /// External S3 access key.
    #[arg(long)]
    pub s3_access_key: Option<String>,

    /// External S3 secret key.
    #[arg(long)]
    pub s3_secret_key: Option<String>,

    /// Enable TLS for proxy/PostgreSQL connections.
    #[arg(long)]
    pub tls: bool,

    /// Overwrite existing `docker-compose.yml` and `.env` if present.
    #[arg(long)]
    pub force: bool,

    /// Immediately start the cluster (`pgvisor up`) after generating config.
    #[arg(long)]
    pub start: bool,
}
