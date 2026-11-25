use std::net::TcpListener;

use crate::docker::command_executor::CommandExecutor;
use crate::error::CliError;

/// Verifies Docker Engine, Docker Compose v2+, and TCP port availability on the host.
pub struct PreflightChecker<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> PreflightChecker<'a> {
    /// Creates a new `PreflightChecker` using the given `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Ensures `docker` CLI, `docker compose` v2+, and the Docker daemon are available.
    pub fn check_docker(&self) -> Result<(), CliError> {
        if !self.executor.is_in_path("docker") {
            return Err(CliError::PreflightFailed(
                "'docker' is required but not installed or not in PATH. Install Docker: https://docs.docker.com/get-docker/".to_string(),
            ));
        }

        let compose_out = self
            .executor
            .run("docker", &["compose", "version"])
            .map_err(|err| CliError::PreflightFailed(err.to_string()))?;
        if !compose_out.success() {
            return Err(CliError::PreflightFailed(format!(
                "'docker compose' (v2+) is required but unavailable: {}",
                compose_out.stderr.trim()
            )));
        }

        let info_out = self
            .executor
            .run("docker", &["info", "--format", "{{.ServerVersion}}"])
            .map_err(|err| CliError::PreflightFailed(err.to_string()))?;
        if !info_out.success() {
            return Err(CliError::PreflightFailed(format!(
                "Docker daemon is not reachable (`docker info` failed): {}",
                info_out.stderr.trim()
            )));
        }

        Ok(())
    }

    /// Checks whether `bind_addr:port` can be bound by attempting a transient `TcpListener::bind`.
    pub fn check_tcp_port(&self, bind_addr: &str, port: u16) -> Result<(), CliError> {
        let listener = TcpListener::bind((bind_addr, port)).map_err(|err| CliError::PortInUse {
            bind_addr: bind_addr.to_string(),
            port,
            reason: err.to_string(),
        })?;
        drop(listener);
        Ok(())
    }
}
