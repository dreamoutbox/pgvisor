# Tests & Compose Architecture

### if you want to change test compose profiles, test port mappings, or concurrent test execution:
- [`docker-compose.yml`](file:///home/z/git/pgvisor/docker-compose.yml): canonical base compose template and single source of truth.
- [`scripts/generate_test_composes.py`](file:///home/z/git/pgvisor/scripts/generate_test_composes.py): generates isolated test compose files with port offsets, project scoping, and 1GB RAM limits on postgres nodes.
- [`scripts/generate-test-composes.sh`](file:///home/z/git/pgvisor/scripts/generate-test-composes.sh): bash wrapper to run the python generator.
- [`composes/docker-compose.*.yml`](file:///home/z/git/pgvisor/composes/): generated per-test isolated compose files (`crud`, `backup-restore`, `pitr`, `failover`, `auto-rejoin`, `rejoin-fenced`, `add-node`, `add-node4`, `switchover`, `users-permissions`).
- [`test.sh`](file:///home/z/git/pgvisor/test.sh): master test suite orchestrator supporting sequential and parallel (`-j N`) execution across all test profiles.
- [`tests/test-cluster-crud.sh`](file:///home/z/git/pgvisor/tests/test-cluster-crud.sh): self-contained demo CRUD test (port 5532).
- [`tests/test-backup-restore.sh`](file:///home/z/git/pgvisor/tests/test-backup-restore.sh): self-contained basebackup & restore test (port 5632).
- [`tests/test-incremental-pitr.sh`](file:///home/z/git/pgvisor/tests/test-incremental-pitr.sh): self-contained incremental PITR test (port 5732).
- [`tests/test-failover.sh`](file:///home/z/git/pgvisor/tests/test-failover.sh): self-contained failover & leader promotion test (port 5832).
- [`tests/test-auto-rejoin.sh`](file:///home/z/git/pgvisor/tests/test-auto-rejoin.sh): self-contained auto-rejoin standby test (port 5932).
- [`tests/test-rejoin-fenced.sh`](file:///home/z/git/pgvisor/tests/test-rejoin-fenced.sh): self-contained fenced quorum lost rejoin test (port 6032).
- [`tests/test-add-node.sh`](file:///home/z/git/pgvisor/tests/test-add-node.sh): self-contained dynamic 4th node scale-out test (port 6132).
- [`tests/test-switchover.sh`](file:///home/z/git/pgvisor/tests/test-switchover.sh): self-contained manual leader switchover test (port 6232).
- [`tests/test-users-permissions.sh`](file:///home/z/git/pgvisor/tests/test-users-permissions.sh): self-contained user CRUD, role membership, and table privilege matrix test (port 6332).
- [`reset-docker-compose.sh`](file:///home/z/git/pgvisor/reset-docker-compose.sh): developer cluster reset script; builds images by default, supports `--no-build` and `-s`/`--silent`.

