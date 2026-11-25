use std::collections::VecDeque;
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::Duration;

use pgvisor_cli::CliError;
use pgvisor_cli::docker::{
    CommandExecutor, CommandOutput, ComposeRunner, HealthWaiter, PreflightChecker,
};
use tempfile::TempDir;

struct MockCommandExecutor {
    docker_in_path: bool,
    responses: Mutex<VecDeque<CommandOutput>>,
    recorded: Mutex<Vec<Vec<String>>>,
}

impl MockCommandExecutor {
    fn new(docker_in_path: bool, responses: Vec<CommandOutput>) -> Self {
        Self {
            docker_in_path,
            responses: Mutex::new(responses.into()),
            recorded: Mutex::new(Vec::new()),
        }
    }

    fn recorded_calls(&self) -> Vec<Vec<String>> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl CommandExecutor for MockCommandExecutor {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CliError> {
        let mut entry = vec![program.to_string()];
        entry.extend(args.iter().map(|s| (*s).to_string()));
        self.recorded.lock().expect("lock recorded").push(entry);

        let next = self
            .responses
            .lock()
            .expect("lock responses")
            .pop_front()
            .unwrap_or(CommandOutput {
                status_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        Ok(next)
    }

    fn run_interactive(&self, program: &str, args: &[&str]) -> Result<i32, CliError> {
        let out = self.run(program, args)?;
        Ok(out.status_code)
    }

    fn is_in_path(&self, program: &str) -> bool {
        program == "docker" && self.docker_in_path
    }
}

fn ok_out(stdout: &str) -> CommandOutput {
    CommandOutput {
        status_code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn err_out(code: i32, stderr: &str) -> CommandOutput {
    CommandOutput {
        status_code: code,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

#[test]
fn preflight_fails_when_docker_not_in_path() {
    let mock = MockCommandExecutor::new(false, vec![]);
    let checker = PreflightChecker::new(&mock);
    let err = checker.check_docker().expect_err("missing docker");
    assert!(matches!(err, CliError::PreflightFailed(_)));
    assert!(err.to_string().contains("not installed"));
}

#[test]
fn preflight_fails_when_compose_missing_or_daemon_unreachable() {
    let mock_compose = MockCommandExecutor::new(true, vec![err_out(1, "unknown command")]);
    let err = PreflightChecker::new(&mock_compose)
        .check_docker()
        .expect_err("compose missing");
    assert!(matches!(err, CliError::PreflightFailed(_)));
    assert!(err.to_string().contains("docker compose"));

    let mock_daemon = MockCommandExecutor::new(
        true,
        vec![ok_out("Docker Compose version v2.29.0"), err_out(1, "Cannot connect")],
    );
    let err = PreflightChecker::new(&mock_daemon)
        .check_docker()
        .expect_err("daemon down");
    assert!(matches!(err, CliError::PreflightFailed(_)));
    assert!(err.to_string().contains("not reachable"));
}

#[test]
fn preflight_detects_port_in_use() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();

    let mock = MockCommandExecutor::new(true, vec![]);
    let checker = PreflightChecker::new(&mock);
    let err = checker
        .check_tcp_port("127.0.0.1", port)
        .expect_err("port in use");
    assert!(matches!(err, CliError::PortInUse { port: p, .. } if p == port));
}

#[test]
fn compose_runner_includes_env_file_when_present() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").expect("write compose");
    std::fs::write(dir.path().join(".env"), "POSTGRES_USER=postgres\n").expect("write env");

    let mock = MockCommandExecutor::new(true, vec![ok_out("")]);
    let runner = ComposeRunner::new(&mock, dir.path());
    runner.up_detached().expect("up_detached");

    let calls = mock.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains(&"--env-file".to_string()));
    assert!(calls[0].ends_with(&["up".to_string(), "-d".to_string()]));
}

#[test]
fn health_waiter_converges_after_starting() {
    let dir = TempDir::new().expect("tempdir");
    let mock = MockCommandExecutor::new(
        true,
        vec![
            // Poll 1: service_container_id -> running + starting
            ok_out("cid-node1\n"),
            ok_out("running\n"),
            ok_out("starting\n"),
            // Poll 2: service_container_id -> running + healthy
            ok_out("cid-node1\n"),
            ok_out("running\n"),
            ok_out("healthy\n"),
        ],
    );
    let compose = ComposeRunner::new(&mock, dir.path());
    let waiter = HealthWaiter::new(&mock, Duration::from_millis(1), Duration::from_secs(2));
    waiter
        .wait_for_services(&compose, &["pgvisor-node1".to_string()])
        .expect("converges to healthy");
}

#[test]
fn health_waiter_fails_fast_on_exited_container_with_logs() {
    let dir = TempDir::new().expect("tempdir");
    let mock = MockCommandExecutor::new(
        true,
        vec![
            ok_out("cid-node2\n"),
            ok_out("exited\n"),
            ok_out("unhealthy\n"),
            ok_out("FATAL: password authentication failed\n"),
        ],
    );
    let compose = ComposeRunner::new(&mock, dir.path());
    let waiter = HealthWaiter::new(&mock, Duration::from_millis(1), Duration::from_secs(2));
    let err = waiter
        .wait_for_services(&compose, &["pgvisor-node2".to_string()])
        .expect_err("exited container");
    match err {
        CliError::ContainerExited { name, state, logs } => {
            assert_eq!(name, "pgvisor-node2");
            assert_eq!(state, "exited");
            assert!(logs.contains("password authentication failed"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn health_waiter_times_out_with_status_summary() {
    let dir = TempDir::new().expect("tempdir");
    let mock = MockCommandExecutor::new(
        true,
        vec![
            ok_out("cid-node1\n"),
            ok_out("running\n"),
            ok_out("starting\n"),
        ],
    );
    let compose = ComposeRunner::new(&mock, dir.path());
    let waiter = HealthWaiter::new(&mock, Duration::from_millis(1), Duration::from_millis(0));
    let err = waiter
        .wait_for_services(&compose, &["pgvisor-node1".to_string()])
        .expect_err("timeout");
    match err {
        CliError::HealthTimeout { details, .. } => {
            assert!(details.contains("pgvisor-node1"));
            assert!(details.contains("status=running"));
            assert!(details.contains("health=starting"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
