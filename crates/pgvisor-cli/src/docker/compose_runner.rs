use std::path::{Path, PathBuf};

use crate::docker::command_executor::CommandExecutor;
use crate::docker::command_output::CommandOutput;
use crate::error::CliError;

/// Wraps `docker compose -f <dir>/docker-compose.yml` invocations.
pub struct ComposeRunner<'a> {
    executor: &'a dyn CommandExecutor,
    compose_file: PathBuf,
    env_file: Option<PathBuf>,
}

impl<'a> ComposeRunner<'a> {
    /// Creates a `ComposeRunner` targeting `<dir>/docker-compose.yml` and optional `<dir>/.env`.
    pub fn new(executor: &'a dyn CommandExecutor, dir: &Path) -> Self {
        let compose_file = dir.join("docker-compose.yml");
        let candidate_env = dir.join(".env");
        let env_file = if candidate_env.is_file() {
            Some(candidate_env)
        } else {
            None
        };
        Self {
            executor,
            compose_file,
            env_file,
        }
    }

    /// Returns the path to `docker-compose.yml`.
    pub fn compose_file(&self) -> &Path {
        &self.compose_file
    }

    fn base_args(&self) -> Vec<String> {
        let mut args = vec![
            "compose".to_string(),
            "-f".to_string(),
            self.compose_file.to_string_lossy().into_owned(),
        ];
        if let Some(env_path) = self.env_file.as_ref() {
            args.push("--env-file".to_string());
            args.push(env_path.to_string_lossy().into_owned());
        }
        args
    }

    fn run_compose(&self, sub_args: &[&str]) -> Result<CommandOutput, CliError> {
        let mut owned = self.base_args();
        for arg in sub_args {
            owned.push((*arg).to_string());
        }
        let arg_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let output = self.executor.run("docker", &arg_refs)?;
        if !output.success() {
            return Err(CliError::CommandFailed {
                command: format!("docker {}", arg_refs.join(" ")),
                status_code: output.status_code,
                stderr: output.stderr.trim().to_string(),
            });
        }
        Ok(output)
    }

    fn run_compose_interactive(&self, sub_args: &[&str]) -> Result<i32, CliError> {
        let mut owned = self.base_args();
        for arg in sub_args {
            owned.push((*arg).to_string());
        }
        let arg_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.executor.run_interactive("docker", &arg_refs)
    }

    /// Starts the cluster in detached mode (`docker compose up -d`).
    pub fn up_detached(&self) -> Result<(), CliError> {
        self.run_compose(&["up", "-d"])?;
        Ok(())
    }

    /// Stops cluster containers while preserving volumes (`docker compose down`).
    pub fn down(&self) -> Result<(), CliError> {
        self.run_compose(&["down"])?;
        Ok(())
    }

    /// Stops cluster containers and removes volumes (`docker compose down -v --remove-orphans`).
    pub fn clean(&self) -> Result<(), CliError> {
        self.run_compose(&["down", "-v", "--remove-orphans"])?;
        Ok(())
    }

    /// Restarts cluster containers (`docker compose restart`).
    pub fn restart(&self) -> Result<(), CliError> {
        self.run_compose(&["restart"])?;
        Ok(())
    }

    /// Displays container status interactively (`docker compose ps`).
    pub fn ps(&self) -> Result<(), CliError> {
        self.run_compose_interactive(&["ps"])?;
        Ok(())
    }

    /// Streams or prints container logs (`docker compose logs`).
    pub fn logs(
        &self,
        follow: bool,
        tail: Option<u32>,
        services: &[String],
    ) -> Result<(), CliError> {
        let mut owned = vec!["logs".to_string()];
        if follow {
            owned.push("-f".to_string());
        }
        if let Some(lines) = tail {
            owned.push("--tail".to_string());
            owned.push(lines.to_string());
        }
        for svc in services {
            owned.push(svc.clone());
        }
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.run_compose_interactive(&refs)?;
        Ok(())
    }

    /// Pulls service images (`docker compose pull`).
    pub fn pull(&self) -> Result<(), CliError> {
        self.run_compose(&["pull"])?;
        Ok(())
    }

    /// Builds service images (`docker compose build`).
    pub fn build(&self) -> Result<(), CliError> {
        self.run_compose(&["build"])?;
        Ok(())
    }

    /// Validates compose syntax and interpolation (`docker compose config -q`).
    pub fn config_check(&self) -> Result<(), CliError> {
        self.run_compose(&["config", "-q"])?;
        Ok(())
    }

    /// Resolves the container ID for a compose service via `docker compose ps -a -q <service>`.
    pub fn service_container_id(&self, service: &str) -> Result<Option<String>, CliError> {
        let out = self.run_compose(&["ps", "-a", "-q", service])?;
        let trimmed = out.stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(
                trimmed.lines().next().unwrap_or(trimmed).trim().to_string(),
            ))
        }
    }

    /// Returns `true` if `image` is already present in the local Docker image cache.
    pub fn image_exists(&self, image: &str) -> Result<bool, CliError> {
        let out = self.executor.run("docker", &["image", "inspect", image])?;
        Ok(out.success())
    }
}
