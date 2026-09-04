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

- [ ] write script to test failover. remove node1 (leader). then expected the cluster to promote node2 or node3 to be leader.

- [ ] add audit logs view in web dashboard for node up/down. backup/restore perform.

- [ ] add page for manage databaser users and permissions.

---

- [ ] - **Dynamic Cluster Scaling**: Protocol for adding and removing sidecar nodes dynamically via OpenRaft joint consensus at runtime without node restarts.

---

# Backlog:

- [ ] make sidecar worker not access the backup storage directly. (remove `S3_ENDPOINT` `S3_BUCKET` `S3_ACCESS_KEY` `S3_SECRET_KEY`). make proxy generate presigned url for backup/restore.
