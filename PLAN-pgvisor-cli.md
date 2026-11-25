# Plan: Native `pgvisor` CLI & Release Binary Workflow (`TODO.md` L261-L262)

Scope: Build a native Rust `pgvisor` CLI binary (`crates/pgvisor-cli`) modeled on `examples/pgvisorctl.sh` with a server-first `pgvisor setup` workflow (no git clone required on the target host), and add a GitHub Actions release binary build/publish pipeline.

Build & test commands (from repo inspection):
- `cargo check --workspace`
- `cargo nextest run --test-threads 1 --fail-fast -p pgvisor-cli`
- `cargo nextest run --test-threads 1 --fail-fast`
- `./test.sh` (Docker Compose integration tests in `tests/`)

Existing repo context:
- Workspace members in `Cargo.toml`: `pgvisor-core`, `pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard` (all at `0.4.0`, `shared-version = true` via `release.sh` / `cargo-release`).
- Current cluster architecture (`docker-compose.yml`): 3 sidecar nodes (`pgvisor-node1..3`), SQL-only `pgvisor-proxy` (`:5432`), standalone `pgvisor-dashboard` (`:8080` talking to `PGVISOR_SIDECAR_URLS`), and S3 storage (`rustfs` + `scripts/init-s3-bucket.sh`). Note `examples/docker-compose.yml` still reflects the pre-split layout (`PGVISOR_ADMIN_TOKEN`, embedded dashboard); the CLI must generate the current post-split layout.
- `.github/workflows/ci.yml` already runs `test` -> `docker` -> `release` (`softprops/action-gh-release@v2`) on `v*` tag pushes, but currently attaches no CLI binaries to the GitHub Release.
- `Dockerfile` builds only `-p pgvisor-proxy -p pgvisor-sidecar -p pgvisor-dashboard`, so `pgvisor-cli` stays out of the container image; keep `pgvisor-cli` dependencies minimal so `cargo chef cook` stays fast.

## Phases

### Phase 1: Workspace crate scaffold & CLI argument skeleton [STATUS: DONE]
- Goal: Add `crates/pgvisor-cli` producing the `pgvisor` binary with parsed subcommands, global flags, and structured errors.
- Tasks:
  - [x] Add `"crates/pgvisor-cli"` to `workspace.members` and `clap` (derive feature) to `[workspace.dependencies]` in `Cargo.toml`.
  - [x] Create `crates/pgvisor-cli/Cargo.toml` (`edition = "2024"`, `version = "0.4.0"`, `[[bin]] name = "pgvisor"`), depending only on lightweight crates (`clap`, `thiserror`, `anyhow`, `tracing`, `tracing-subscriber`, `hex`, `getrandom` or `rand`).
  - [x] Implement modular crate layout (one type per file, sparse `mod.rs`): `src/lib.rs`, `src/main.rs` (`tracing_subscriber` + `anyhow::Result` boundary), `src/error.rs` (`CliError` via `thiserror`), `src/cli/args.rs` (`CliArgs` with global `-C`/`--dir`, `-s`/`--silent`), and `src/cli/command.rs` (`CliCommand` enum: `Setup`, `Up`, `Down` with alias `stop`, `Restart`, `Status`, `Logs`, `Clean`).
  - [x] Stub command handlers returning structured unimplemented messages so the binary compiles and `--help` / `--version` work end-to-end.
  - [x] Add argument parsing unit tests in `crates/pgvisor-cli/tests/cli_args_test.rs` covering subcommands, aliases, defaults, and invalid options.
- Done when: `cargo check --workspace` passes, `cargo nextest run --test-threads 1 --fail-fast -p pgvisor-cli` passes, and `cargo run -p pgvisor-cli -- --help` lists all subcommands cleanly.

### Phase 2: Server preflight checks & Docker Compose runner abstraction [STATUS: DONE]
- Goal: Encapsulate Docker/Compose invocation, server prerequisite checks, and container health polling behind testable Rust abstractions.
- Tasks:
  - [x] Add `src/docker/command_executor.rs` trait + `SystemCommandExecutor` implementation to allow deterministic unit testing without a live Docker daemon.
  - [x] Add `src/docker/preflight.rs` checking `docker` in `PATH`, `docker compose version` (v2+), Docker daemon availability (`docker info`), and TCP port availability checks for server setup.
  - [x] Add `src/docker/compose_runner.rs` wrapping `docker compose -f <dir>/docker-compose.yml --env-file <dir>/.env` for `up -d`, `down`, `down -v --remove-orphans`, `restart`, `ps`, `logs`, `pull`, `build`, and `config` validation.
  - [x] Add `src/docker/container_status.rs` (`ContainerState` and `ContainerHealth` enums) and `src/docker/health_waiter.rs` polling `docker inspect` with timeout, failing fast with the last 30 container log lines if any required container transitions to `exited` or `dead`.
  - [x] Add unit tests in `crates/pgvisor-cli/tests/docker_runner_test.rs` exercising preflight failures, healthy convergence, fail-fast on dead container, and timeout diagnostics.
- Done when: `cargo check --workspace` and `cargo nextest run --test-threads 1 --fail-fast -p pgvisor-cli` pass.

### Phase 3: Server-first `pgvisor setup` config generator & secret provisioning [STATUS: DONE]
- Goal: Allow an operator on a fresh server (without cloning the repo) to run `pgvisor setup` and generate a secure, self-contained cluster directory.
- Tasks:
  - [x] Add `src/setup/s3_mode.rs` (`S3Mode` enum: `Bundled`, `External`) and `src/setup/setup_options.rs` (`SetupOptions` supporting both interactive TTY prompts and fully non-interactive flags: `--yes`/`--non-interactive`, `--cluster-id`, `--image`, `--proxy-port`, `--dashboard-bind` defaulting to `127.0.0.1`, `--dashboard-port`, `--superuser`, `--s3-mode`, `--s3-endpoint`, `--s3-bucket`, `--s3-region`, `--s3-access-key`, `--s3-secret-key`, `--tls`, `--force`, `--start`).
  - [x] Add `src/setup/secret_generator.rs` generating cryptographically strong random secrets for `POSTGRES_PASSWORD`, `PGVISOR_CLUSTER_SECRET`, `PGVISOR_API_SESSION_SECRET`, and (when bundled) S3 access/secret keys instead of hardcoded dev passwords.
  - [x] Add `src/setup/compose_template.rs` generating a standalone `docker-compose.yml` matching the current architecture: 3 sidecar nodes (`pgvisor-node1..3`) with `PGVISOR_SUPERUSER`, `PGVISOR_CLUSTER_SECRET`, `PGVISOR_API_SESSION_SECRET`, SQL-only `pgvisor-proxy`, standalone `pgvisor-dashboard` bound to `${PGVISOR_DASHBOARD_BIND:-127.0.0.1}:${PGVISOR_DASHBOARD_PORT:-8080}:8080`, and optional bundled `rustfs` (`rustfs/rustfs:latest`) + init job (omitted cleanly in `External` mode). No repo-relative `build:` context.
  - [x] Add `src/setup/env_template.rs` writing `.env` with `0600` Unix permissions and writing embedded `scripts/init-s3-bucket.sh` with `0755` permissions (when bundled S3 is enabled), refusing to clobber existing files unless `--force` is passed.
  - [x] Add `src/setup/setup_command.rs` running server preflight checks, resolving options, writing files, printing next steps (including SSH tunnel hint when dashboard binds to `127.0.0.1`), and optionally chaining directly into `up` when `--start` is passed.
  - [x] Add unit tests in `crates/pgvisor-cli/tests/setup_command_test.rs` (using `tempfile`) asserting `Bundled` vs `External` compose output, random secret generation, `.env` `0600` mode, `init-s3-bucket.sh` `0755` mode, and `--force` overwrite protection.
- Done when: `cargo nextest run --test-threads 1 --fail-fast -p pgvisor-cli` passes and `pgvisor setup --dir <tmp> --non-interactive` produces a compose file accepted by `docker compose -f <tmp>/docker-compose.yml config`.

### Phase 4: Cluster lifecycle commands (`up`, `down`, `restart`, `status`, `logs`, `clean`) [STATUS: DONE]
- Goal: Reach full operational parity with `examples/pgvisorctl.sh` using the generated or existing cluster directory.
- Tasks:
  - [x] Add `src/config/env_file.rs` to parse key-value pairs from `<dir>/.env` safely without invoking a shell, plus compose service discovery to determine which containers require health checks (e.g., excluding bundled S3 when configured externally).
  - [x] Implement `src/commands/up_command.rs`: verify `<dir>/docker-compose.yml` exists (guiding the user to run `pgvisor setup` if not), run preflight checks, pull missing images (or `--build` only when explicitly requested with a local `Dockerfile`), run `docker compose up -d`, wait via `HealthWaiter`, and print connection details (proxy URI, dashboard URL / SSH tunnel hint, `PGVISOR_SUPERUSER` login note, management commands).
  - [x] Implement `src/commands/down_command.rs` (handling `down` and `stop` alias), `src/commands/restart_command.rs` (restart + health wait), `src/commands/status_command.rs` (`docker compose ps`), and `src/commands/logs_command.rs` (streaming `docker compose logs -f` with `--no-follow`, `--tail`, and optional service filters).
  - [x] Implement `src/commands/clean_command.rs` requiring interactive confirmation unless `-y`/`--yes` is set, then running `docker compose down -v --remove-orphans`.
  - [x] Add unit tests in `crates/pgvisor-cli/tests/lifecycle_commands_test.rs` testing `.env` parsing, missing-setup guidance, `clean` confirmation guard, and command argument forwarding.
- Done when: `cargo check --workspace` and `cargo nextest run --test-threads 1 --fail-fast -p pgvisor-cli` pass.

### Phase 5: End-to-end integration test for `pgvisor` CLI [STATUS: DONE]
- Goal: Verify a real server-style setup-to-clean lifecycle outside the repo tree using Docker.
- Tasks:
  - [x] Add `tests/test-cli-setup.sh` (reusing `tests/lib/helper.sh`) that builds `pgvisor`, creates an isolated temporary directory outside the repo compose paths, runs `pgvisor setup --non-interactive --image dreamoutbox/pgvisor:latest` with non-colliding test ports, verifies `.env` permissions (`0600`) and non-default secrets, runs `pgvisor up` and `pgvisor status`, verifies SQL write + polling read through the proxy (per replication-lag testing rules), and verifies `pgvisor restart`, `pgvisor down`, and `pgvisor clean --yes`.
  - [x] Verify `Dockerfile` still builds cleanly with the new workspace member present.
- Done when: `tests/test-cli-setup.sh` passes end-to-end and `cargo nextest run --test-threads 1 --fail-fast` passes.

### Phase 6: GitHub CI release binary workflow [STATUS: DONE]
- Goal: Automatically compile, archive, checksum, and attach `pgvisor` CLI binaries to GitHub Releases on `v*` tags.
- Tasks:
  - [x] Add executable script `scripts/build-cli-release.sh` (`chmod +x`) taking `<target>` and `<version>`, compiling `cargo build --release -p pgvisor-cli --target <target>`, verifying the binary, and packaging `dist/pgvisor-<version>-<target>.tar.gz` plus its `.sha256` file.
  - [x] Update `.github/workflows/ci.yml` to add a `build-cli` matrix job (`needs: [test]`, triggered on `refs/tags/v*` and `workflow_dispatch`) building portable static Linux (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) and macOS (`aarch64-apple-darwin`, `x86_64-apple-darwin`) binaries via `scripts/build-cli-release.sh` and uploading `dist/` artifacts.
  - [x] Update the `release` job in `.github/workflows/ci.yml` (`needs: [test, docker, build-cli]`) to download all CLI artifacts, generate a consolidated `SHA256SUMS` file, and upload `dist/*.tar.gz` and `dist/SHA256SUMS` with `softprops/action-gh-release@v2`.
- Done when: `scripts/build-cli-release.sh` produces a working tarball and valid checksum locally, and `.github/workflows/ci.yml` passes workflow syntax validation.

### Phase 7: Documentation, legacy script note, and path-lookup cache [STATUS: DONE]
- Goal: Document server installation and usage of `pgvisor`, and index the new crate for future agents.
- Tasks:
  - [x] Update `README.md` and `docs/src/content/docs/quick-start.mdx` with GitHub Release download + checksum verification instructions and `pgvisor setup` / `pgvisor up` server quickstart.
  - [x] Update `DEVELOPMENT.md` with `crates/pgvisor-cli` architecture and build/test commands.
  - [x] Add a deprecation/pointer comment in `examples/pgvisorctl.sh` pointing to the native `pgvisor` CLI without breaking `demo-launcher-entrypoint.sh` or `examples/run-demo-app.sh`.
  - [x] Create `.agents/path-lookup/cli.md` and update `.agents/path-lookup/misc.md` (repo-relative paths only), and tick off the CLI item in `TODO.md`.
- Done when: all doc links/commands match the implemented CLI flags, path-lookup entries verify cleanly, and `cargo check --workspace` + `cargo nextest run --test-threads 1 --fail-fast` pass.

## Confirmed decisions & assumptions
- **Bundled S3 service (`rustfs`)**: Confirmed — `pgvisor setup` uses `rustfs/rustfs:latest` with service name `rustfs` (consistent with `scripts/init-s3-bucket.sh`).
- **Release target matrix**: Confirmed — build static Linux binaries (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) and macOS binaries (`aarch64-apple-darwin`, `x86_64-apple-darwin`) in the CLI release workflow.
- **Default install directory & dashboard bind**: Confirmed — `pgvisor setup` defaults to `--dir .` (overridable, e.g. `/opt/pgvisor`) and binds `pgvisor-dashboard` to `127.0.0.1:8080` by default for server safety (with `--dashboard-bind 0.0.0.0` opt-in).
- **Scope boundary**: Confirmed — database/backup/restore management via CLI (`TODO.md` Backlog) is excluded from this plan; this plan focuses on server cluster bootstrap (`setup`), Docker lifecycle (`up`/`down`/`restart`/`status`/`logs`/`clean`), and CI binary releases.
- **Legacy script compatibility**: Confirmed — `examples/pgvisorctl.sh` is referenced by `demo-launcher-entrypoint.sh` and `examples/run-demo-app.sh`; keep it intact for now and migrate those scripts in a follow-up.
