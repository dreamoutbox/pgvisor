# Tests & Compose Architecture

### If you want to change test compose profiles, test port mappings, or concurrent test execution, then check:

- `docker-compose.yml` = canonical base compose template and single source of truth.
- `scripts/generate_test_composes.py` = generates isolated test compose files with port offsets, project scoping, and 1 CPU + 1GB RAM limits on containers.
- `scripts/generate-test-composes.sh` = bash wrapper to run the python generator.
- `composes/docker-compose.*.yml` = generated per-test isolated compose files (`crud`, `backup-restore`, `pitr`, `failover`, `auto-rejoin`, `rejoin-fenced`, `add-node`, `add-node4`, `switchover`, `users-permissions`, `transaction`, `routing`).
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
- `tests/test-routing.sh` = self-contained read/write routing assertion test (port 6532; plain SELECT to replica, DDL/DML to leader, in-txn SELECT pinned to leader, round-robin standby read load-balancing to node3, node2-down failover to node3).
- `tests/test-audit-logs.sh` = self-contained audit logs verification test (port 6632; node up/down, dangerous SQL with PITR, backups, elections, user/role management, S3 storage).
- `tests/test-double-failure.sh` = self-contained double-failure disaster recovery test (port 6732; 2 nodes down, quorum loss prevents writes, sequential restart with standby rejoin, data integrity & WAL streaming).
- `tests/test-proxy-failover.sh` = self-contained dual-proxy redundancy test (proxy1: 6832, proxy2: 6833; stop proxy1 and assert continued DB access via proxy2, verify recovery).
- `tests/test-timeline-divergence.sh` = self-contained timeline divergence and recovery target overrun test (port 7032; PITR restore f1 -> incr2 -> re-restore f1, asserts no standby crash, no recovery overrun fatal, and fast /tables load).
- `tests/test-node-lifecycle.sh` = self-contained node start/stop/restart verification test (port 7132; tests stopping standby, pool continuity, starting standby, restart, error handling 400/404, and audit events).
- `tests/test-restart-leader.sh` = self-contained stopped leader restart and standby rejoin test (port 7232; stops leader Node 1, waits for standby failover, starts Node 1, asserts no split-brain crash, and verifies replication catch-up).
- `tests/test-promoted-restore.sh` = self-contained promoted leader snapshot restore regression test (port 7332; stops leader Node 1, asserts Node 2 auto-promotes, restores incremental snapshot without PITR target time, asserts Node 2 boots as read-write primary with zero recovery spam, and verifies replica re-sync).
- `composes/docker-compose.proxy-failover-proxy2.yml` = compose overlay defining the second proxy (pgvisor-proxy2) on ports 6833/9481.
- `reset-docker-compose.sh` = developer cluster reset script; builds images by default, supports `--no-build` and `-s`/`--silent`.
- `dev-dump-logs.sh` = developer helper script to dump logs from pgvisor nodes 1-3 into `logs/` directory, supporting timestamps, tail, and proxy/minio options.
- `dev-setup-demo-data.sh` = developer helper script to seed demo schema and initial records, take full backup 'f1', execute delayed inserts (echo, foxtrot, golf), and trigger incremental backup 'incr2'.
- `scripts/dev-setup-demo-data.sh` = symlink pointing to root `dev-setup-demo-data.sh`.

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

### If you want to fix flaky post-restore assertions (wrong row counts after snapshot restore), then check:

- `tests/test-incremental-pitr.sh` = PITR test; uses `wait_for_healthy` and `wait_for_proxy_ready` after `restore_cluster_node`, and ensures `pg_switch_wal()` executes on the leader.
- `crates/pgvisor-sidecar/src/supervisor.rs` = `ProcessStatus::Restoring` state set during `restore_from_snapshot` and `resync_from_primary` to pause standby elections during active restores.
- `crates/pgvisor-sidecar/src/main.rs` = sidecar `/control/status` handler exposing `"restoring"` status and heartbeat loop honoring `"restoring"` leader status to prevent split-brain elections.
- `crates/pgvisor-proxy/src/backup.rs` = `ProxyBackupService::restore_backup` re-syncing both dynamic and configured standbys with 3-attempt retry loop.
- `crates/pgvisor-proxy/src/main.rs` = proxy discovery loop preserving discovered leader while in `"restoring"` status and passing configured standbys.
- `tests/test-backup-restore.sh` = similar restore+verify pattern; apply the same retry budget if it shows similar flakiness.
- `tests/lib/cluster.sh` = `wait_for_proxy_ready` and `wait_for_healthy` container health helpers.

### If you want to fix flaky test assertions from read/write splitting, replication lag, or standby timeline divergence, then check:

- `crates/pgvisor-sidecar/src/config.rs` = omits `recovery_target_timeline = 'current'` and `restore_command` for streaming standbys so standbys can follow promoted leaders to new timelines without archive conflicts.
- `tests/test-failover.sh` = polling retry loop for post-failover row count verification via proxy.
- `tests/test-auto-rejoin.sh` = polling retry loop for post-failover write row count verification via proxy.
- `tests/test-switchover.sh` = 10-attempt polling retry budget for streaming replication across standbys after primary switchovers.
- `tests/test-double-failure.sh` = polling retry loops for `COUNT_T1` and `COUNT_T2` proxy row counts after quorum rejoin.
- `tests/test-add-node.sh` = polling retry loop for post-scale proxy write verification.
- `tests/test-timeline-divergence.sh` = `assert_golf_count` polling helper preventing microsecond race conditions across round-robin standbys.
