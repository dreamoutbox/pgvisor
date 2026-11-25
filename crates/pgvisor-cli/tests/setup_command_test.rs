use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;

use pgvisor_cli::docker::{CommandExecutor, CommandOutput};
use pgvisor_cli::setup::{ComposeTemplate, EnvTemplate, SecretGenerator, SetupCommand, SetupOptions};
use pgvisor_cli::{CliError, S3Mode, SetupArgs};
use tempfile::TempDir;

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

struct ReadyDockerExecutor;

impl CommandExecutor for ReadyDockerExecutor {
    fn run(&self, _program: &str, _args: &[&str]) -> Result<CommandOutput, CliError> {
        Ok(CommandOutput {
            status_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
        })
    }

    fn run_interactive(&self, _program: &str, _args: &[&str]) -> Result<i32, CliError> {
        Ok(0)
    }

    fn is_in_path(&self, program: &str) -> bool {
        program == "docker"
    }
}

fn base_setup_args(mode: S3Mode) -> SetupArgs {
    SetupArgs {
        non_interactive: true,
        cluster_id: Some("test-cluster".to_string()),
        image: Some("dreamoutbox/pgvisor:latest".to_string()),
        proxy_port: Some(free_port()),
        dashboard_bind: "127.0.0.1".to_string(),
        dashboard_port: Some(free_port()),
        superuser: Some("postgres".to_string()),
        s3_mode: Some(mode),
        s3_api_port: Some(free_port()),
        s3_console_port: Some(free_port()),
        s3_endpoint: if mode == S3Mode::External {
            Some("https://s3.example.com".to_string())
        } else {
            None
        },
        s3_bucket: Some("my-backups".to_string()),
        s3_region: Some("us-east-1".to_string()),
        s3_access_key: if mode == S3Mode::External {
            Some("ext-access".to_string())
        } else {
            None
        },
        s3_secret_key: if mode == S3Mode::External {
            Some("ext-secret".to_string())
        } else {
            None
        },
        tls: false,
        force: false,
        start: false,
    }
}

#[test]
fn bundled_setup_generates_rustfs_compose_random_secrets_and_permissions() {
    let dir = TempDir::new().expect("tempdir");
    let args = base_setup_args(S3Mode::Bundled);
    SetupCommand::new(&ReadyDockerExecutor)
        .execute(dir.path(), &args, true)
        .expect("setup bundled");

    let compose = std::fs::read_to_string(dir.path().join("docker-compose.yml")).expect("compose");
    assert!(compose.contains("rustfs:"));
    assert!(compose.contains("rustfs-init:"));
    assert!(compose.contains("pgvisor-node1:"));
    assert!(compose.contains("pgvisor-proxy:"));
    assert!(compose.contains("pgvisor-dashboard:"));
    assert!(!compose.contains("build:"));
    assert!(!compose.contains("container_name:"));

    let env = std::fs::read_to_string(dir.path().join(".env")).expect("env");
    assert!(env.contains("PGVISOR_CLUSTER_ID=test-cluster"));
    assert!(env.contains("PGVISOR_DASHBOARD_BIND=127.0.0.1"));
    assert!(env.contains("S3_ENDPOINT=http://rustfs:9000"));
    assert!(!env.contains("POSTGRES_PASSWORD=postgres\n"));

    let env_mode = std::fs::metadata(dir.path().join(".env"))
        .expect("env meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(env_mode, 0o600);

    let init_script = dir.path().join("scripts/init-s3-bucket.sh");
    assert!(init_script.is_file());
    let script_mode = std::fs::metadata(&init_script)
        .expect("script meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(script_mode, 0o755);
}

#[test]
fn external_s3_omits_rustfs_and_init_script() {
    let dir = TempDir::new().expect("tempdir");
    let args = base_setup_args(S3Mode::External);
    let options = SetupOptions::resolve(&args).expect("resolve external");
    let secrets = SecretGenerator::generate_all().expect("secrets");
    EnvTemplate::write_cluster_files(dir.path(), &options, &secrets).expect("write external");

    let compose = ComposeTemplate::render(&options);
    assert!(!compose.contains("rustfs:"));
    assert!(!compose.contains("rustfs-init:"));
    assert!(!compose.contains("rustfs_data:"));
    assert!(!dir.path().join("scripts/init-s3-bucket.sh").exists());

    let env = std::fs::read_to_string(dir.path().join(".env")).expect("env");
    assert!(env.contains("S3_ENDPOINT=https://s3.example.com"));
    assert!(env.contains("S3_ACCESS_KEY=ext-access"));
    assert!(env.contains("S3_SECRET_KEY=ext-secret"));
}

#[test]
fn secret_generator_produces_distinct_secrets_across_runs() {
    let s1 = SecretGenerator::generate_all().expect("s1");
    let s2 = SecretGenerator::generate_all().expect("s2");
    assert_ne!(s1.postgres_password, s2.postgres_password);
    assert_ne!(s1.cluster_secret, s2.cluster_secret);
    assert_ne!(s1.api_session_secret, s2.api_session_secret);
    assert_ne!(s1.s3_secret_key, s2.s3_secret_key);
}

#[test]
fn setup_refuses_overwrite_without_force_and_allows_with_force() {
    let dir = TempDir::new().expect("tempdir");
    let mut args = base_setup_args(S3Mode::Bundled);
    SetupCommand::new(&ReadyDockerExecutor)
        .execute(dir.path(), &args, true)
        .expect("first setup");

    let err = SetupCommand::new(&ReadyDockerExecutor)
        .execute(dir.path(), &args, true)
        .expect_err("second setup without force");
    assert!(matches!(err, CliError::FileAlreadyExists(_)));

    args.force = true;
    SetupCommand::new(&ReadyDockerExecutor)
        .execute(dir.path(), &args, true)
        .expect("setup with force");
}
