# Tests & Compose Architecture

### If you want to change test compose profiles, test port mappings, or concurrent test execution, then check:

- `docker-compose.yml` = canonical base compose template and single source of truth.
- `scripts/generate_test_composes.py` = generates isolated test compose files with port offsets, project scoping, and 1GB RAM limits on postgres nodes.
- `scripts/generate-test-composes.sh` = bash wrapper to run the python generator.
- `composes/docker-compose.*.yml` = generated per-test isolated compose files (`crud`, `backup-restore`, `pitr`, `failover`, `auto-rejoin`, `rejoin-fenced`, `add-node`, `add-node4`, `switchover`, `users-permissions`, `transaction`).
- `test.sh` = master test suite orchestrator supporting sequential and parallel (`-j N`) execution across all test profiles.
- `tests/test-cluster-crud.sh` = self-contained demo CRUD test (port 5532).
- `tests/test-backup-restore.sh` = self-contained basebackup & restore test (port 5632).
- `tests/test-incremental-pitr.sh` = self-contained incremental PITR test (port 5732).
- `tests/test-failover.sh` = self-contained failover & leader promotion test (port 5832).
- `tests/test-auto-rejoin.sh` = self-contained auto-rejoin standby test (port 5932).
- `tests/test-rejoin-fenced.sh` = self-contained fenced quorum lost rejoin test (port 6032).
- `tests/test-add-node.sh` = self-contained dynamic 4th node scale-out test (port 6132).
- `tests/test-switchover.sh` = self-contained manual leader switchover test (port 6232).
- `tests/test-users-permissions.sh` = self-contained user CRUD, role membership, and table privilege matrix test (port 6332).
- `tests/test-transaction.sh` = self-contained BEGIN/COMMIT, BEGIN/ROLLBACK, and error-mid-tx+ROLLBACK verification test (port 6432).
- `scripts/test-transaction.sql` = SQL fixture run inside test-transaction.sh; exercises all 3 transaction scenarios.
- `reset-docker-compose.sh` = developer cluster reset script; builds images by default, supports `--no-build` and `-s`/`--silent`.

### If you want to add a new integration test (new test script + compose profile), then check:

- `scripts/generate_test_composes.py` = add a tuple to PROFILES list (name, suffix, proxy_port, dash_port, minio_api, minio_console).
- `scripts/generate-test-composes.sh` = run this to regenerate compose files after editing the python generator.
- `test.sh` = add the new `test-<name>.sh` filename to the TEST_SCRIPTS array.
- `tests/test-<name>.sh` = create the shell script following the existing pattern (isolated project name, port constants, cleanup trap, retry-loop assertions).
- `scripts/test-<name>.sql` (optional) = create a companion SQL fixture if the test drives SQL directly.

### If you want to change test cluster startup, health check wait, or container cleanup, then check:

- `tests/lib/cluster.sh` = shared library for `cluster_up` (`--progress quiet`), `cluster_down`, `wait_and_remove_minio_init`, `wait_for_healthy` (with fail-fast crash detection), and `wait_for_proxy_ready`.
- `reset-docker-compose.sh` = developer cluster reset script with `pgvisor-minio-init` wait & removal and container healthcheck polling with timeout.
- `tests/test-*.sh` = integration test scripts that invoke `cluster_up`, `wait_for_healthy`, and `cluster_down`.
