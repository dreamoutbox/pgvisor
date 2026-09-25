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

- [x] change postgres version to 18

- [x] use external rust crate for postgres protocol if exists. no need to manually implement this. use https://crates.io/crates/pgwire

- [x] chore: show "Optional Label / Note" in table at "Basebackups & Snapshots" page.

- [x] running all tests

- [x] add web dashboard auth.

- [x] test adding new node (`tests/test-add-node.sh`)
      - start cluster with 3 nodes (node1 leader, node2/node3 standbys)
      - seed baseline table and records via proxy (`t0_pre_scale`)
      - launch pgvisor-node4 container connected to leader with PGVISOR_ROLE=standby
      - assert node4 sidecar reports status="running" and role="standby"
      - assert node4 cloned existing data (`t0_pre_scale` present via pg_basebackup)
      - assert leader pg_stat_replication shows 3 active WAL sender connections
      - write new record (`t1_post_scale`) through proxy
      - assert node4 receives streaming replication (both records present, pg_is_in_recovery=t)
      - assert node4 stays standby without spurious elections over several heartbeat intervals
      - teardown node4 container and volume cleanly

- [x] web dashboard still show pg version as 16.3 after change to `postgres:18.6-bookworm` in Dockerfile.

- [x] make test run concurrent. use custom pre-define ports for each tests.

- [x] manually switchover new leader. dev can perform switchover in web dashboard.

- [x] add page for manage databaser users and permissions.

- [x] add transaction SQL test

- [x] chore: remove verbose in test logs

- [x] assert write query always go to leader and read only go to replica.

- [x] add audit logs page in web dashboard for 
      - node up/down. 
      - backup/restore perform. 
      - dangerous SQL (DROP TABLE/TRUNCATE/DELETE) logging. so when it happens, we can use the time to restore with PITR. 
      - log worker election vote. and election result.
      - log worker join, leave event.
      - user create/delete/grant permission

- [x] very bad disaster testing: 3 nodes setup. 2 nodes down.
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

- [x] chore: limit CPU and RAM in test docker composes

- [x] testing 2 proxy, 3 nodes setup. then stop proxy1 and assert still access DB with proxy2.

- [x] make node 3 accept read, not standby when node 2 down.

- [x] add README.md

- [x] add github workflow to build docker image, run tests, and publish to docker hub. 

- [x] web dashboard chart & graph for useful data
      - all nodes uptime
      - node read/write count
      - node cpu/memory usage
      - proxy read/write count
      - backup size & throughput
      - backup success/fail rate
      - replication lag

- [x] fix Failed Tests:
      1st run:      
            - test-failover.sh (38s)  
            - test-auto-rejoin.sh (34s)  
            - test-switchover.sh (43s) 
            - test-timeline-divergence.sh (202s) 
      2nd run:
            - test-failover.sh (30s)
            - test-auto-rejoin.sh (30s)
            - test-switchover.sh (39s)
            - test-double-failure.sh (50s)
      3rd run:
            - test-failover.sh (33s)
            - test-auto-rejoin.sh (35s)
            - test-switchover.sh (45s)
            - test-add-node.sh (45s)
            - test-timeline-divergence.sh (204s)

      run test with `./test.sh -j 5`
      different failed tests in different runs come from following flaky tests:
      - test-timeline-divergence.sh
      - test-double-failure.sh
      - test-add-node.sh 

- [x] start/stop/restart node from web dashboard.

- [x] add log text to highligt `start/stop/backup/restore/listen to new leader` with input value 

- [x] fix cluster failure and recovery log spam when restoring snapshot on auto-promoted leader (verified in `test-promoted-restore.sh`) 

- [x] make following config-able with ENV: 
      - backup snapshots keep count. retention days.
      - CRON auto run full backup / incremental backup.
- [x] mutex lock on backup/restore prevent concurrent actions. like 2 users clicking backup/restore on web dashboard on the same time.
- [x] add test. assert restore perform on primary. backup perform on follower nodes to reduce the primary node load.

- [x] loadtesting. how much we can handle. (implemented in `dev-loadtest.sh`, outputs reports to `tests/load/`)

- [x] fix tests/test-switchover.sh

- [x] make backup snapshot ID include the optional label.
- [x] add backup metadata.json. inside backup dir.

- [ ] user manually upload backup file to s3, then restore with web dashboard.

- [x] prevent invalid PITR input error make database can’t start (error → offline hang not restart). 

- [x] store multiple audit logs in a json file. rotate if too large.
- [x] add pagination to Cluster Audit Logs. make table row high smaller.

- [x] demo PgVisor.
    - [x] setup.
    - [x] adding new node.
    - [x] backup/restore
    - [x] testing node down.

- [x] split code into smaller files
      - `crates/pgvisor-sidecar/src/main.rs`
      - `crates/pgvisor-dashboard/src/handlers.rs`

- [x] secure proxy to sidecar & sidecar to sidecar communication. when communication over network with password. currently sidecar rust axum server not have any authentication.

- [x] TLS/SSL mode. auto setup. write simple shell/python script to test SSL/TLS connection is working.

- [x] add wiki

- [x] demo: simple app connect and use pgvisor
      - Python MVC web app with Bootstrap 5 in `examples/demo_app`
      - Containerized with Dockerfile and docker-compose.yml
      - Demonstrates L7 read/write splitting, CRUD, and cluster diagnostics

- [x] bug: failover not triggering when stop leader node (node1)

- [x] fix tests

- [x] fix `Latency StdDev (ms)` in loadtest report as 0

- [ ] inspect each node logs, postgresql config, pg_hba, etc in web dashboard

- [ ] edit/delete tables rows from web dashboard

- [ ] secure openraft port and communication. use shared secret ENV.

- [ ] install PG extensions with web dashboard

- [ ] add demo images in README and Wiki

---

# Backlog

- [ ] docker swarm testing

- [ ] make proxy and sidecar export opentelemetry data. to use with prometheus/grafana.

- [ ] web dashboard support multiple users.

- [ ] manage database and execute backup/restore with cli

- [ ] redesign when new node join cluster.
      - dynamic node discovery. use env ROLE / PEERS list as starter data.
      - new node send join cluster request to leader node
      - leader node register new node
      - leader node replicate (send all) cluster data (all nodes data/cluster data/status/etc.) to all nodes. incase the leader failed, so new leader can take over with up to date data.

- [ ] multiple s3 storage.

- [ ] survive. Multi-Availability Zone (Multi-AZ) support

- [ ] add Kubernetes Operator & CRDs. - **Kubernetes Operator**: Custom CRDs and k8s-native controllers.

- [ ] make sidecar worker not access the backup storage directly. (remove `S3_ENDPOINT` `S3_BUCKET` `S3_ACCESS_KEY` `S3_SECRET_KEY`). make proxy generate presigned url for backup/restore.

---

# Chore

- [x] move duplicate helper functions in tests/*.sh to single place. search duplicated functions by regex. move to tests/lib/helper.sh

- [ ] move rust tests to separate files/tests directory.

- [ ] run `cargo test` use cargo-nextest to run tests. one by one. fail fast.

- [ ] web dashboard change to use bootstrap5. less custom css/js.
