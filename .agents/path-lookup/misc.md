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
