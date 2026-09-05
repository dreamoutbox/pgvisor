# TODO

- [x] assert MVP working.
- [x] web dashboard. add page to list/view table data. (like adminer, but simpler)
- [x] the web dashboard should have menu and page for viewing available backups.
- [x] we should able to backup/restore with web dashboard.

- [x] write script to test backup/restore.
- [x] script to test full base backup snapshot (`tests/test-backup-restore.sh`).
      - create table t1.
      - insert 'alpha' into t1.
      - backup at T0.
      - list backup, check backup has 'alpha'.
      - drop t1.
      - restore from backup at T1.
      - check table has 'alpha' in T1.

- [x] script to test incremental backup (PITR). 
      - create simple test table. 
      - add 'alpha' to table. 
      - backup at T0. 
      - add 'beta' at T1. 
      - backup at T2.
      - restore from T0 backup at T3.
      - check table has 'alpha'
      - restore from T2 backup at T4.
      - check table has 'alpha' and 'beta' in T4.

- [x] write script to test failover (`tests/test-failover.sh`).
      - verify node1 is primary, node2/node3 are standbys, write baseline data at T0.
      - stop node1 (leader).
      - assert cluster promotes node2 or node3 to read-write primary.
      - assert proxy transparently routes new writes at T1 to the promoted leader.
      - assert surviving standby replicates from the new leader.
      - restart node1 and verify split-brain prevention (rejoins as standby).

- [x] investigate rejoin node (from docker container remove/stop) Supervision Status is "Fenced (Quorum Lost)" and role is "Standby (Replica)". the rejoin (old node1 leader) should join the cluster as a standby replica.

- [x] change postgres version to 17/18

- [x] use external rust crate for postgres protocol if exists. no need to manually implement this. use https://crates.io/crates/pgwire

- [x] chore: show "Optional Label / Note" in table at "Basebackups & Snapshots" page.

- [ ] add web dashboard auth.

- [ ] manually switchover new leader

- [ ] very bad disaster testing: 3 nodes setup. 2 nodes down.
      - start as 3 nodes 
      - down node1, node2
      - assert only node3 left.
      - try write to it. expected fail.
      - start back node1.
      - assert node1 rejoin as standby.
      - assert database cluster is working, data is ok and have leader.
      - start back node2.
      - assert node2 rejoin as standby.
      - assert database cluster is working, data is ok and have leader.

- [ ] test adding new node

- [ ] add audit logs view in web dashboard for node up/down. backup/restore perform. dangerous SQL (DROP TABLE/TRUNCATE/DELETE) logging. so when it happens, we can use the time to restore with PITR.

- [ ] add page for manage databaser users and permissions.



- [ ] add README.md

- [ ] testing two proxy. 3 nodes. then stop proxy1 and assert still access DB with proxy2.

- [ ] run `cargo test` use cargo-nextest to run tests. one by one. fail fast.

---

# Backlog:

- [ ] web dashboard change to use bootstrap5. less custom css/js.

- [ ] web dashboard chart & graph

- [ ] - **Dynamic Cluster Scaling**: Protocol for adding and removing sidecar nodes dynamically via OpenRaft joint consensus at runtime without node restarts.

- [ ] make sidecar worker not access the backup storage directly. (remove `S3_ENDPOINT` `S3_BUCKET` `S3_ACCESS_KEY` `S3_SECRET_KEY`). make proxy generate presigned url for backup/restore.

- [ ] add Kubernetes Operator & CRDs
