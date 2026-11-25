### If you want to modify `pgvisor` CLI arguments, `pgvisor setup` config generation, or cluster lifecycle commands (`up`, `down`, `restart`, `status`, `logs`, `clean`), then check:

- `crates/pgvisor-cli/Cargo.toml` = CLI crate manifest and lightweight dependency list
- `crates/pgvisor-cli/src/main.rs` = Binary entrypoint (`tracing_subscriber` + `anyhow::Result`)
- `crates/pgvisor-cli/src/lib.rs` = Top-level command dispatch (`run`)
- `crates/pgvisor-cli/src/error.rs` = `CliError` enum (`thiserror`)
- `crates/pgvisor-cli/src/cli/` = Clap argument structs (`CliArgs`, `CliCommand`, `SetupArgs`, `UpArgs`, `RestartArgs`, `LogsArgs`, `CleanArgs`, `S3Mode`)
- `crates/pgvisor-cli/src/docker/` = `CommandExecutor`, `PreflightChecker`, `ComposeRunner`, and `HealthWaiter`
- `crates/pgvisor-cli/src/setup/` = `SetupCommand`, `SetupOptions`, `SecretGenerator`, `ComposeTemplate`, and `EnvTemplate`
- `crates/pgvisor-cli/src/config/` = `EnvFile` parser and `ClusterServices` discovery
- `crates/pgvisor-cli/src/commands/` = `UpCommand`, `DownCommand`, `RestartCommand`, `StatusCommand`, `LogsCommand`, and `CleanCommand`
- `crates/pgvisor-cli/tests/` = Unit tests (`cli_args_test.rs`, `docker_runner_test.rs`, `setup_command_test.rs`, `lifecycle_commands_test.rs`)
- `tests/test-cli-setup.sh` = End-to-end Docker Compose lifecycle test for `pgvisor setup` -> `up` -> `clean`
- `dev-build-cli.sh` = Dev helper script to compile the `pgvisor` CLI binary (`--release` optional)
- `dev-run-cli.sh` = Dev wrapper script running `target/<profile>/pgvisor`, invoking `dev-build-cli.sh` if missing
