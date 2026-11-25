use std::path::PathBuf;

use clap::Parser;
use pgvisor_cli::{CliArgs, CliCommand, S3Mode};

#[test]
fn parses_default_global_flags_and_up_defaults() {
    let args = CliArgs::try_parse_from(["pgvisor", "up"]).expect("parse up");
    assert_eq!(args.dir, PathBuf::from("."));
    assert!(!args.silent);
    match args.command {
        CliCommand::Up(up) => {
            assert!(!up.build);
            assert_eq!(up.timeout, 90);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_global_dir_and_silent_flags() {
    let args = CliArgs::try_parse_from(["pgvisor", "-C", "/opt/pgvisor", "-s", "status"])
        .expect("parse status");
    assert_eq!(args.dir, PathBuf::from("/opt/pgvisor"));
    assert!(args.silent);
    assert!(matches!(args.command, CliCommand::Status));
}

#[test]
fn parses_stop_alias_for_down() {
    let args = CliArgs::try_parse_from(["pgvisor", "stop"]).expect("parse stop alias");
    assert!(matches!(args.command, CliCommand::Down));
}

#[test]
fn parses_setup_flags_and_defaults() {
    let args = CliArgs::try_parse_from([
        "pgvisor",
        "setup",
        "--non-interactive",
        "--cluster-id",
        "prod-db",
        "--s3-mode",
        "external",
        "--s3-endpoint",
        "https://s3.example.com",
        "--s3-bucket",
        "backups",
        "--tls",
        "--force",
        "--start",
    ])
    .expect("parse setup");

    match args.command {
        CliCommand::Setup(setup) => {
            assert!(setup.non_interactive);
            assert_eq!(setup.cluster_id.as_deref(), Some("prod-db"));
            assert_eq!(setup.dashboard_bind, "127.0.0.1");
            assert_eq!(setup.s3_mode, Some(S3Mode::External));
            assert_eq!(
                setup.s3_endpoint.as_deref(),
                Some("https://s3.example.com")
            );
            assert_eq!(setup.s3_bucket.as_deref(), Some("backups"));
            assert!(setup.tls);
            assert!(setup.force);
            assert!(setup.start);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_logs_with_flags_and_services() {
    let args = CliArgs::try_parse_from([
        "pgvisor",
        "logs",
        "--no-follow",
        "--tail",
        "50",
        "pgvisor-node1",
        "pgvisor-proxy",
    ])
    .expect("parse logs");

    match args.command {
        CliCommand::Logs(logs) => {
            assert!(logs.no_follow);
            assert_eq!(logs.tail, Some(50));
            assert_eq!(logs.services, vec!["pgvisor-node1", "pgvisor-proxy"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_clean_and_restart_flags() {
    let clean = CliArgs::try_parse_from(["pgvisor", "clean", "-y"]).expect("parse clean");
    assert!(matches!(
        clean.command,
        CliCommand::Clean(c) if c.yes
    ));

    let restart = CliArgs::try_parse_from(["pgvisor", "restart", "--timeout", "120"])
        .expect("parse restart");
    assert!(matches!(
        restart.command,
        CliCommand::Restart(r) if r.timeout == 120
    ));
}

#[test]
fn rejects_unknown_subcommand_or_flag() {
    assert!(CliArgs::try_parse_from(["pgvisor", "nonexistent"]).is_err());
    assert!(CliArgs::try_parse_from(["pgvisor", "up", "--bogus"]).is_err());
}
