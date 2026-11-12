# Plan: Debug Enhance (TODO.md L240-246)

Scope: backup JSON gains last transaction time/xid; three dev scripts for S3 snapshot export/import/inspect.

Build/test: `cargo check`, `cargo test -p pgvisor-core`, `./test.sh` (shell integration tests in `tests/`, plain-text output, poll for replica lag).

Existing code (from exploration):
- `BasebackupMeta` in `crates/pgvisor-core/src/backup/manager.rs` is the `basebackups/<snapshot-id>.json` schema. It is also injected as `metadata.json` in the archive by `process_basebackup_archive` (`archive.rs`).
- `ProxyBackupService::create_backup` in `crates/pgvisor-proxy/src/backup.rs` runs `pg_basebackup`, builds the meta, and saves it.
- Dev scripts live at repo root (`dev-*.sh`, bash, `set -euo pipefail`). `debug/` is gitignored. `scripts/init-s3-bucket.sh` shows the curl SigV4 pattern against the compose S3 (`S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`).

## Phases

### Phase 1: Add last transaction fields to backup metadata [STATUS: TODO]
- Goal: `BasebackupMeta` carries `last_transaction_time` and `last_transaction_xid`, serialized optionally and backward compatible.
- Tasks:
  - [ ] Add `last_transaction_time: Option<DateTime<Utc>>` and `last_transaction_xid: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Init to `None` in `BasebackupMeta::new`. Add a `with_last_transaction` builder.
  - [ ] Unit tests: old JSON without the fields still deserializes. A round trip with the fields works.
  - [ ] Update `dashboard` `BackupItemView` mapping only if it constructs `BasebackupMeta` literally (fix compile errors, no UI change).
- Done when: `cargo check` and `cargo test -p pgvisor-core` pass.

### Phase 2: Capture last transaction at backup time [STATUS: TODO]
- Goal: `create_backup` records the last committed transaction time and xid in the JSON and in `metadata.json`.
- Tasks:
  - [ ] Decide the source (see risks). Default: on the leader, query `pg_last_committed_xact()` (`xid`, `timestamp`) just after `pg_basebackup` finishes. If commit timestamps are not tracked, fall back to `pg_current_xact_id()`/`txid_current_snapshot()` xmax for xid and `now()` for time, and mark the value as approximate.
  - [ ] Enable `track_commit_timestamp = on` in the sidecar generated config (`crates/pgvisor-sidecar/src/config.rs`) if that source is chosen. Needs a restart, so check the initdb and config path.
  - [ ] Add a helper in `crates/pgvisor-proxy/src/backup.rs` that runs the query and returns `Option<(DateTime<Utc>, String)>`. A failed query logs a warning and leaves the fields `None`, so the backup never fails. The simulated-snapshot path sets plausible values.
  - [ ] Set the fields on `meta` before `process_basebackup_archive` serializes `metadata.json`, and before `save_basebackup`.
  - [ ] Extend `tests/test-backup-restore.sh`: after creating a backup, poll the S3 JSON and assert the fields are present and the time is no later than now and no earlier than the backup start.
- Done when: `cargo check` passes, unit tests pass, and `./test.sh` backup-restore passes. A new backup JSON shows both fields, and the time is usable as a PITR target.

### Phase 3: dev-take-backups-snapshot-s3.sh and dev-import-backups-snapshot-s3.sh [STATUS: TODO]
- Goal: Dump the S3 bucket to `debug-s3-bucket-snapshot/` and restore it from there, for reproducing bugs.
- Tasks:
  - [ ] Create a shared helper `scripts/lib/s3-common.sh` (endpoint and credential env defaults, SigV4 curl wrapper, list objects, get, put). Shared by all three scripts.
  - [ ] `dev-take-backups-snapshot-s3.sh`: list all keys in the bucket (paginated, `list-type=2`), download each to `debug-s3-bucket-snapshot/<key>`, replace the old dir, and print a count. Support `--dir`, `--bucket`, `--endpoint` overrides. Plain text only.
  - [ ] `dev-import-backups-snapshot-s3.sh`: ensure the bucket exists (reuse `scripts/init-s3-bucket.sh`), optionally empty it (`--clean`, off by default), and upload every file preserving key paths. Fail with a clear message if the dir is missing.
  - [ ] Add `debug-s3-bucket-snapshot/` to `.gitignore`. `chmod +x`.
  - [ ] Test script (`tests/test-dev-s3-snapshot.sh`): put sample objects, take, wipe, import, and assert identical keys and contents via a polling-safe check.
- Done when: a take, wipe, import round trip reproduces the bucket exactly, and the scripts pass shellcheck if available.

### Phase 4: dev-inspect-backup.sh [STATUS: TODO]
- Goal: Print name, label, create_time, last_transaction_time and last_transaction_xid for backups.
- Tasks:
  - [ ] `dev-inspect-backup.sh [snapshot-id]`: read `clusters/<cluster>/basebackups/*.json` from S3 (via `s3-common.sh`) or from `--dir debug-s3-bucket-snapshot` offline. No argument lists all, newest first. Use `jq` and fail with a clear message if it is missing.
  - [ ] Output a plain-text aligned table. Show `-` for fields missing in old backups.
  - [ ] Options: `--cluster` (default `pgvisor-cluster`), `--dir`, `--json`.
  - [ ] Add a case to the Phase 3 test script that inspects a sample JSON, including one without the new fields.
- Done when: running against a fresh backup shows all five fields, and old backups render with `-`.

### Phase 5: Docs and path-lookup [STATUS: TODO]
- Goal: Document the fields and scripts.
- Tasks:
  - [ ] Mention the new fields and scripts in `DEVELOPMENT.md`.
  - [ ] Update `.agents/path-lookup/backup.md` and `misc.md` (repo-relative paths).
  - [ ] Tick the three items in `TODO.md`.
- Done when: docs build links and paths are valid, and the full `./test.sh` passes.

## Unknowns, risks, assumptions
- Last transaction time source: `pg_last_committed_xact()` needs `track_commit_timestamp=on` (restart required). It reports only the latest commit, so it can be later than the basebackup stop point. For PITR the safe value is the commit time at or before backup stop. Assumption: take it right after `pg_basebackup` ends, and treat it as an upper bound. The alternative is parsing the commit record from WAL (`pg_waldump`), which is heavier. Confirm which you prefer.
- Existing clusters will not have commit timestamps until a restart, so the fallback in Phase 2 is needed.
- `jq` and `curl --aws-sigv4` (curl 7.75+) are assumed available on the host. S3 is the compose-provided one (the compose file still says minio; the scripts use `S3_*` env vars so the backend does not matter).
- Scripts do not copy WAL specially: WAL archive objects live in the same bucket and are covered by the full dump.

