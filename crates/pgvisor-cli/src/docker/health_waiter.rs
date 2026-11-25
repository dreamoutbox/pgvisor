use std::thread;
use std::time::{Duration, Instant};

use crate::docker::command_executor::CommandExecutor;
use crate::docker::compose_runner::ComposeRunner;
use crate::docker::container_health::ContainerHealth;
use crate::docker::container_state::ContainerState;
use crate::error::CliError;

/// Polls Docker container status and healthchecks until all required services are healthy.
pub struct HealthWaiter<'a> {
    executor: &'a dyn CommandExecutor,
    poll_interval: Duration,
    timeout: Duration,
}

impl<'a> HealthWaiter<'a> {
    /// Creates a `HealthWaiter` with explicit poll interval and timeout.
    pub fn new(
        executor: &'a dyn CommandExecutor,
        poll_interval: Duration,
        timeout: Duration,
    ) -> Self {
        Self {
            executor,
            poll_interval,
            timeout,
        }
    }

    /// Waits for every service in `required_services` to report `healthy`.
    pub fn wait_for_services(
        &self,
        compose: &ComposeRunner<'_>,
        required_services: &[String],
    ) -> Result<(), CliError> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let mut all_healthy = true;
            let mut snapshots = Vec::with_capacity(required_services.len());

            for service in required_services {
                let target = compose
                    .service_container_id(service)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| service.clone());

                let (state, health) = self.inspect_container(&target);
                if state.is_terminal_failure() {
                    let logs = self.tail_logs(&target);
                    return Err(CliError::ContainerExited {
                        name: service.clone(),
                        state: state.as_str().to_string(),
                        logs,
                    });
                }

                if health != ContainerHealth::Healthy {
                    all_healthy = false;
                }
                snapshots.push((service.as_str(), state, health));
            }

            if all_healthy {
                return Ok(());
            }

            if Instant::now() >= deadline {
                let details = snapshots
                    .into_iter()
                    .map(|(svc, state, health)| {
                        format!("  - {svc}: status={}, health={}", state.as_str(), health.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(CliError::HealthTimeout {
                    timeout_secs: self.timeout.as_secs(),
                    details,
                });
            }

            thread::sleep(self.poll_interval);
        }
    }

    fn inspect_container(&self, target: &str) -> (ContainerState, ContainerHealth) {
        let state_out = self.executor.run(
            "docker",
            &["inspect", "-f", "{{.State.Status}}", target],
        );
        let state = match state_out {
            Ok(out) if out.success() => ContainerState::parse_output(&out.stdout),
            _ => ContainerState::Missing,
        };

        let health_out = self.executor.run(
            "docker",
            &[
                "inspect",
                "-f",
                "{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}",
                target,
            ],
        );
        let health = match health_out {
            Ok(out) if out.success() => ContainerHealth::parse_output(&out.stdout),
            _ => ContainerHealth::None,
        };

        (state, health)
    }

    fn tail_logs(&self, target: &str) -> String {
        match self.executor.run("docker", &["logs", "--tail", "30", target]) {
            Ok(out) => {
                let combined = format!("{}{}", out.stdout, out.stderr);
                if combined.trim().is_empty() {
                    "<no container logs available>".to_string()
                } else {
                    combined
                }
            }
            Err(_) => "<unable to retrieve container logs>".to_string(),
        }
    }
}
