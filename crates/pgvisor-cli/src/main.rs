use clap::Parser;
use pgvisor_cli::CliArgs;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = CliArgs::parse();
    pgvisor_cli::run(args)?;
    Ok(())
}
