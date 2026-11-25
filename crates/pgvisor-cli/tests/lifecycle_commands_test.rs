use std::collections::VecDeque;
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use pgvisor_cli::commands::{
    CleanCommand, DownCommand, LogsCommand, RestartCommand, StatusCommand, UpCommand,
};
use pgvisor_cli::config::{ClusterServices, EnvFile};
use pgvisor_cli::docker::{CommandExecutor, CommandOutput};
use pgvisor_cli::{CleanArgs, CliError, LogsArgs, RestartArgs, UpArgs};
use tempfile::TempDir;

struct MockExecutor {
    responses: Mutex<VecDeque<CommandOutput>>,
    recorded: Mutex<Vec<Vec<String>>>,
}

impl MockExecutor {
    fn new(responses: Vec<CommandOutput>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            recorded: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.recorded.lock().expect("lock calls").clone()
    }
}

impl CommandExecutor for MockExecutor {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CliError> {
        let mut entry = vec![program.to_string()];
        entry.extend(args.iter().map(|s| (*s).to_string()));
        self.recorded.lock().expect("lock calls").push(entry);

        Ok(self
            .responses
            .lock()
            .expect("lock responses")
            .pop_front()
            .unwrap_or(CommandOutput {
                status_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }))
    }

    fn run_interactive(&self, program: &str, args: &[&str]) -> Result<i32, CliError> {
        let out = self.run(program, args)?;
        Ok(out.status_code)
    }

    fn is_in_path(&self, program: &str) -> bool {
        program == "docker"
    }
}

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        status_code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn err(code: i32, stderr: &str) -> CommandOutput {
    CommandOutput {
        status_code: code,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

#[test]
fn env_file_parses_quotes_comments_and_export() {
    let raw = r#"
# Comment line
POSTGRES_USER="postgres"
export POSTGRES_PASSWORD='secret=val'
PGVISOR_PROXY_PORT=15432
"#;
    let parsed = EnvFile::parse(raw, Path::new(".env")).expect("parse env");
    assert_eq!(parsed.get("POSTGRES_USER"), Some("postgres"));
    assert_eq!(parsed.get("POSTGRES_PASSWORD"), Some("secret=val"));
    assert_eq!(parsed.get("PGVISOR_PROXY_PORT"), Some("15432"));

    let bad = EnvFile::parse("INVALID_LINE", Path::new(".env")).expect_err("invalid env");
    assert!(matches!(bad, CliError::InvalidEnvFile { line_number: 1, .. }));
}

#[test]
fn cluster_services_detects_bundled_rustfs_vs_external_s3() {
    let dir = TempDir::new().expect("tempdir");
    let bundled = dir.path().join("bundled.yml");
    std::fs::write(
        &bundled,
        "services:\n  rustfs:\n  pgvisor-node1:\n  pgvisor-node2:\n  pgvisor-node3:\n",
    )
    .expect("write bundled");
    assert_eq!(
        ClusterServices::required_for_compose(&bundled).expect("bundled services"),
        vec!["rustfs", "pgvisor-node1", "pgvisor-node2", "pgvisor-node3"]
    );

    let external = dir.path().join("external.yml");
    std::fs::write(
        &external,
        "services:\n  pgvisor-node1:\n  pgvisor-node2:\n  pgvisor-node3:\n",
    )
    .expect("write external");
    assert_eq!(
        ClusterServices::required_for_compose(&external).expect("external services"),
        vec!["pgvisor-node1", "pgvisor-node2", "pgvisor-node3"]
    );
}

#[test]
fn commands_return_missing_compose_file_when_not_initialized() {
    let dir = TempDir::new().expect("tempdir");
    let mock = MockExecutor::new(vec![]);
    let err = UpCommand::new(&mock)
        .execute(
            dir.path(),
            &UpArgs {
                build: false,
                timeout: 10,
            },
            true,
        )
        .expect_err("missing compose");
    assert!(matches!(err, CliError::MissingComposeFile(_)));
    assert!(err.to_string().contains("pgvisor setup"));
}

#[test]
fn clean_command_requires_confirmation_unless_yes_is_set() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").expect("compose");

    let mock = MockExecutor::new(vec![]);
    let mut no_input = Cursor::new(b"n\n");
    let removed = CleanCommand::new(&mock)
        .execute_with_reader(dir.path(), &CleanArgs { yes: false }, true, &mut no_input)
        .expect("cancel clean");
    assert!(!removed);
    assert!(mock.calls().is_empty());

    let mut yes_input = Cursor::new(b"yes\n");
    let removed = CleanCommand::new(&mock)
        .execute_with_reader(dir.path(), &CleanArgs { yes: false }, true, &mut yes_input)
        .expect("confirm clean");
    assert!(removed);
    assert_eq!(mock.calls().len(), 1);
    assert!(mock.calls()[0].ends_with(&[
        "down".to_string(),
        "-v".to_string(),
        "--remove-orphans".to_string(),
    ]));

    let mock_yes = MockExecutor::new(vec![]);
    let removed = CleanCommand::new(&mock_yes)
        .execute(dir.path(), &CleanArgs { yes: true }, true)
        .expect("clean --yes");
    assert!(removed);
    assert_eq!(mock_yes.calls().len(), 1);
}

#[test]
fn up_pulls_missing_image_starts_and_waits_for_health() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("docker-compose.yml"),
        "services:\n  pgvisor-node1:\n  pgvisor-node2:\n  pgvisor-node3:\n",
    )
    .expect("compose");
    std::fs::write(dir.path().join(".env"), "PGVISOR_IMAGE=custom/pgvisor:1.0\n").expect("env");

    let mock = MockExecutor::new(vec![
        ok("Docker Compose version v2.29.0"),
        ok("27.0.0"),
        err(1, "No such image"), // image_exists -> false -> triggers pull
        ok(""),                  // pull
        ok(""),                  // up -d
        // Health checks for node1, node2, node3
        ok("cid-1\n"),
        ok("running\n"),
        ok("healthy\n"),
        ok("cid-2\n"),
        ok("running\n"),
        ok("healthy\n"),
        ok("cid-3\n"),
        ok("running\n"),
        ok("healthy\n"),
    ]);

    UpCommand::new(&mock)
        .with_poll_interval(Duration::from_millis(1))
        .execute(
            dir.path(),
            &UpArgs {
                build: false,
                timeout: 5,
            },
            true,
        )
        .expect("up succeeds");

    let calls = mock.calls();
    assert!(calls.iter().any(|c| c.ends_with(&["pull".to_string()])));
    assert!(calls.iter().any(|c| c.ends_with(&["up".to_string(), "-d".to_string()])));
}

#[test]
fn down_restart_status_and_logs_forward_expected_compose_args() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("docker-compose.yml"),
        "services:\n  pgvisor-node1:\n",
    )
    .expect("compose");

    let mock = MockExecutor::new(vec![
        ok(""),        // down
        ok(""),        // restart
        ok("cid-1\n"), // service_container_id
        ok("running\n"),
        ok("healthy\n"),
        ok(""), // ps
        ok(""), // logs
    ]);

    DownCommand::new(&mock)
        .execute(dir.path(), true)
        .expect("down");
    RestartCommand::new(&mock)
        .with_poll_interval(Duration::from_millis(1))
        .execute(dir.path(), &RestartArgs { timeout: 5 }, true)
        .expect("restart");
    StatusCommand::new(&mock).execute(dir.path()).expect("status");
    LogsCommand::new(&mock)
        .execute(
            dir.path(),
            &LogsArgs {
                no_follow: true,
                tail: Some(25),
                services: vec!["pgvisor-node1".to_string()],
            },
        )
        .expect("logs");

    let calls = mock.calls();
    assert!(calls[0].ends_with(&["down".to_string()]));
    assert!(calls[1].ends_with(&["restart".to_string()]));
    assert!(calls[5].ends_with(&["ps".to_string()]));
    assert!(calls[6].ends_with(&[
        "logs".to_string(),
        "--tail".to_string(),
        "25".to_string(),
        "pgvisor-node1".to_string(),
    ]));
}
