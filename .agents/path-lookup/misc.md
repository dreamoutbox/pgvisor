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
