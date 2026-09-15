### If you want to run or modify end-to-end integration tests and cluster resets, then check:

- `test.sh` = Main sequential test runner script executing all `tests/*.sh` (supports optional `--build`)
- `tests/run-all-tests.sh` = Concurrent test runner executing isolated compose projects across custom pre-defined port ranges in parallel
- `composes/` = Dedicated Docker compose profiles per test suite (`crud`, `backup-restore`, `pitr`, `failover`, `auto-rejoin`, `rejoin-fenced`, `add-node`)
- `reset-docker-compose.sh` = Docker compose reset helper wiping volumes and restarting fresh cluster containers (supports optional `--build`)
- `tests/test-cluster-crud.sh` = Baseline CRUD test against proxy endpoint
- `tests/test-backup-restore.sh` = Full physical basebackup and snapshot restore verification
- `tests/test-incremental-pitr.sh` = Point-in-time recovery (PITR) test across WAL deltas and multi-stage snapshots
- `tests/test-failover.sh` = Leader crash, quorum election, failover routing, and rejoin verification
- `tests/test-auto-rejoin.sh` = Standby reconnection and automatic rejoin after partition
- `tests/test-rejoin-fenced.sh` = Fenced leader rejoin as standby replica verification
- `tests/test-add-node.sh` = Dynamic scale-out test adding node4 to 3-node cluster and verifying replication

### If you want to modify GitHub Actions CI/CD workflows, automated testing, or Docker Hub publishing, then check:

- `.github/workflows/ci.yml` = GitHub Actions workflow executing cargo check/test, test.sh integration suite, and Docker build/publish to Docker Hub.
- `docker-compose.yml` = Canonical Docker Compose template configuring local Postgres cluster, RustFS S3 storage, and pgvisor-proxy.
- `scripts/init-s3-bucket.sh` = S3 bucket initialization script creating pgvisor-backups in RustFS via curl AWS SigV4.
- `Dockerfile` = Multi-stage Docker build recipe for `pgvisor-sidecar`, `pgvisor-proxy`, and `pgvisor-dashboard`.
- `test.sh` = Integration test orchestrator invoked by the CI test job.
- `README.md` = Documentation for CI status badges, Docker Hub images, and required GitHub repository secrets (`DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`).
- `TODO.md` = Roadmap and task completion tracking for CI/CD and release automation.

### If you want to modify Docker image builds, dependency caching, or cargo-chef layers, then check:

- `Dockerfile` = Multi-stage Docker build recipe using `cargo-chef` (`chef`, `planner`, `builder`) and runtime PostgreSQL 18.6 base.
- `.dockerignore` = Excluded files and directories to avoid invalidating Docker build context and cargo-chef cache layers.
- `Cargo.toml` = Root workspace manifest whose dependencies determine the chef `recipe.json` cache key.
- `Cargo.lock` = Lockfile tracked for deterministic cargo-chef dependency recipe computation.

### If you want to modify user-facing quickstart, consumer setup, or example compose files, then check:

- `examples/docker-compose.yml` = Production-ready 3-node HA cluster compose template with S3 backup storage and L7 proxy
- `examples/setup.sh` = Interactive and automated cluster bootstrap & management script (start, stop, restart, status, clean)
- `examples/.env.example` = Template of configurable environment variables (ports, tokens, credentials, S3 endpoints)
- `examples/scripts/init-s3-bucket.sh` = S3 bucket initialization script mounted into minio-init container
- `setup.sh` = Root wrapper script delegating to `examples/setup.sh`
- `README.md` = Consumer Getting Started instructions, connection details, and dashboard guide
- `DEVELOPMENT.md` = Developer prerequisites, build/test commands, crate layout, tech stack, and configuration reference

### If you want to modify lifecycle highlight logging (start, stop, backup, restore, listen to new leader, become leader), then check:

- `crates/pgvisor-core/src/logging.rs` = highlight banner formatting for `START NODE`, `STOP NODE`, `BACKUP WITH ...`, `RESTORE WITH ...`, `LEADER NODE ... IS DOWN. LISTENING TO NEW LEADER NODE ...`, and `LEADER NODE ... IS DOWN. NOW I (...) BECOME LEADER`
- `crates/pgvisor-core/src/lib.rs` = module exports for `log_highlight`, `extract_node_name`, and formatters
- `crates/pgvisor-sidecar/src/supervisor.rs` = `START NODE` and `STOP NODE` process supervisor highlights
- `crates/pgvisor-sidecar/src/main.rs` = `handle_restore` restore highlight, `handle_repoint` / `start_postgres_safely` / auto-rejoin failover listening highlights, and election auto-promote / `handle_promote` become leader highlights
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::create_backup` backup highlight and `restore_backup` restore highlight
- `crates/pgvisor-proxy/src/main.rs` = proxy dynamic topology monitor failover listening highlight
- `crates/pgvisor-proxy/src/cluster.rs` = proxy node start/stop and switchover highlights
- `crates/pgvisor-dashboard/src/handlers.rs` = standalone backup and cluster service highlights
