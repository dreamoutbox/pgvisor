use std::path::Path;
use std::time::Duration;

use crate::cli::UpArgs;
use crate::config::{ClusterServices, EnvFile};
use crate::docker::{CommandExecutor, ComposeRunner, HealthWaiter, PreflightChecker};
use crate::error::CliError;

/// Executes `pgvisor up` to start the cluster and wait for healthchecks.
pub struct UpCommand<'a> {
    executor: &'a dyn CommandExecutor,
    poll_interval: Duration,
}

impl<'a> UpCommand<'a> {
    /// Creates an `UpCommand` with the default 1-second health poll interval.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self {
            executor,
            poll_interval: Duration::from_secs(1),
        }
    }

    /// Overrides the healthcheck poll interval (primarily for deterministic tests).
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    /// Verifies setup files, runs preflight checks, starts containers, and prints connection info.
    pub fn execute(&self, dir: &Path, args: &UpArgs, silent: bool) -> Result<(), CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }

        PreflightChecker::new(self.executor).check_docker()?;
        let env = EnvFile::load_optional(&dir.join(".env"))?;
        let image = env.get_or("PGVISOR_IMAGE", "dreamoutbox/pgvisor:latest");

        if args.build {
            if !dir.join("Dockerfile").is_file() {
                return Err(CliError::InvalidOption(format!(
                    "`--build` requires a Dockerfile in {}",
                    dir.display()
                )));
            }
            if !silent {
                println!("Building PgVisor Docker images from local source...");
            }
            compose.build()?;
        } else if !compose.image_exists(image)? {
            if !silent {
                println!("Pulling '{image}' from registry...");
            }
            compose.pull()?;
        }

        if !silent {
            println!("Starting PgVisor cluster...");
        }
        compose.up_detached()?;

        let required_services = ClusterServices::required_for_compose(compose.compose_file())?;
        if !silent {
            println!("Waiting for cluster services to become healthy...");
        }
        let waiter = HealthWaiter::new(
            self.executor,
            self.poll_interval,
            Duration::from_secs(args.timeout),
        );
        waiter.wait_for_services(&compose, &required_services)?;

        if !silent {
            Self::print_ready_banner(&env);
        }
        Ok(())
    }

    fn print_ready_banner(env: &EnvFile) {
        let proxy_port = env.get_or("PGVISOR_PROXY_PORT", "5432");
        let dash_bind = env.get_or("PGVISOR_DASHBOARD_BIND", "127.0.0.1");
        let dash_port = env.get_or("PGVISOR_DASHBOARD_PORT", "8080");
        let db_user = env.get_or("POSTGRES_USER", "postgres");
        let db_pass = env.get_or("POSTGRES_PASSWORD", "<configured-in-.env>");
        let superuser = env.get_or("PGVISOR_SUPERUSER", db_user);

        println!();
        println!("=========================================================");
        println!("  PgVisor PostgreSQL HA Cluster is Ready!");
        println!("=========================================================");
        println!("  - PostgreSQL L7 Proxy:  localhost:{proxy_port}");
        println!("  - Web Management UI:    http://{dash_bind}:{dash_port}");
        if dash_bind == "127.0.0.1" {
            println!("  - SSH Tunnel Command:   ssh -L {dash_port}:127.0.0.1:{dash_port} <user>@<server>");
        }
        println!();
        println!("--- Connection Details ---");
        println!("  psql -h localhost -p {proxy_port} -U {db_user} -d postgres");
        println!("  postgresql://{db_user}:{db_pass}@localhost:{proxy_port}/postgres");
        println!();
        println!("--- Dashboard Login ---");
        println!("  Allowed superuser(s): {superuser} (password in .env)");
        println!();
        println!("--- Cluster Management ---");
        println!("  pgvisor status | logs | restart | down | clean");
        println!("=========================================================");
    }
}
