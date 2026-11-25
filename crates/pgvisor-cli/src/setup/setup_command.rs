use std::path::Path;

use crate::cli::{S3Mode, SetupArgs};
use crate::docker::{CommandExecutor, PreflightChecker};
use crate::error::CliError;
use crate::setup::env_template::EnvTemplate;
use crate::setup::secret_generator::SecretGenerator;
use crate::setup::setup_options::SetupOptions;

/// Executes `pgvisor setup` to provision a standalone server cluster directory.
pub struct SetupCommand<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> SetupCommand<'a> {
    /// Creates a `SetupCommand` using the provided `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Runs preflight checks, resolves options, generates secrets, and writes cluster files.
    pub fn execute(&self, dir: &Path, args: &SetupArgs, silent: bool) -> Result<SetupOptions, CliError> {
        let preflight = PreflightChecker::new(self.executor);
        preflight.check_docker()?;

        let options = SetupOptions::resolve(args)?;
        preflight.check_tcp_port("0.0.0.0", options.proxy_port)?;
        preflight.check_tcp_port(&options.dashboard_bind, options.dashboard_port)?;
        if options.s3_mode == S3Mode::Bundled {
            preflight.check_tcp_port("127.0.0.1", options.s3_api_port)?;
            preflight.check_tcp_port("127.0.0.1", options.s3_console_port)?;
        }

        let secrets = SecretGenerator::generate_all()?;
        EnvTemplate::write_cluster_files(dir, &options, &secrets)?;

        if !silent {
            println!("Provisioned PgVisor cluster configuration in {}", dir.display());
            println!("  - Cluster ID:     {}", options.cluster_id);
            println!("  - Proxy Port:     {}", options.proxy_port);
            println!(
                "  - Dashboard Bind: {}:{}",
                options.dashboard_bind, options.dashboard_port
            );
            if options.dashboard_bind == "127.0.0.1" {
                println!(
                    "  - SSH Tunnel:     ssh -L {}:127.0.0.1:{} <user>@<server>",
                    options.dashboard_port, options.dashboard_port
                );
            }
            println!();
            println!("Next step:");
            println!("  pgvisor -C {} up", dir.display());
        }

        if options.start {
            return Err(CliError::NotImplemented("setup --start"));
        }

        Ok(options)
    }
}
