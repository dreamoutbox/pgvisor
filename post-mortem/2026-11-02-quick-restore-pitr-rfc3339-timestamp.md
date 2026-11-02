# Postmortem: Quick Restore PITR Rejects RFC 3339 Timestamp in postgresql.conf

**Date:** 2026-11-02
**Severity:** Medium
**Status:** Resolved

## Summary
Triggering Point-In-Time Recovery via `POST /api/backups/quick-restore` with an ISO-8601/RFC 3339 timestamp (`2026-11-02T00:29:36Z`) failed with HTTP 500 because PostgreSQL rejected the `recovery_target_time` value in `postgresql.conf` on startup. Normalizing the timestamp to PostgreSQL's expected format in both `BackupService::quick_restore` and `ConfigGenerator::write_conf_only` resolved the failure.

## Impact
Any PITR operation invoked via `/api/backups/quick-restore` (or directly via the sidecar `/control/restore` endpoint) using an RFC 3339 timestamp with `T`/`Z` separators caused the leader PostgreSQL process to exit with status 1 during recovery startup, aborting the restore. The sidecar cleanup logic removed `recovery.signal` and restarted the node cleanly after the failed attempt.

## Timeline
- `00:29 UTC` — Demo cluster initialized via `./dev-mock-data.sh`; full backup `f1`, incremental inserts, and incremental backup `incr2` created.
- `00:30 UTC` — `POST /api/backups/quick-restore` invoked with `{"recovery_target_time": "2026-11-02T00:29:36Z"}`.
- `00:30 UTC` — Leader sidecar (`node1`) extracted snapshot `f1`, wrote `recovery_target_time = '2026-11-02T00:29:36Z'` to `postgresql.conf`, and started PostgreSQL; PostgreSQL logged `invalid value for parameter "recovery_target_time": "2026-11-02T00:29:36Z"` and exited with status 1.
- `00:36 UTC` — Root cause identified from `logs/node1.log`, fix applied to dashboard and sidecar crates, and unit tests added.

## Root Cause
PostgreSQL's GUC configuration parser for `recovery_target_time` does not accept RFC 3339 timestamps containing the `T` date-time separator and `Z` suffix (`2026-11-02T00:29:36Z`); it requires a space separator and explicit timezone offset/name (e.g. `2026-11-02 00:29:36 UTC` or `2026-11-02 00:29:36+00`).

While `api_restore_backup` (`POST /api/backups/:id/restore`) parsed the user input via `parse_target_timestamp` and formatted the resulting `DateTime<Utc>` as `%Y-%m-%d %H:%M:%S UTC` before calling `restore_backup`, `BackupService::quick_restore` called `parse_target_timestamp` only for validation and snapshot selection, then passed the raw unformatted `target_time` string down to `restore_backup`. Additionally, `ConfigGenerator::write_conf_only` in `pgvisor-sidecar` interpolated `config.recovery_target_time` verbatim into `postgresql.conf` without normalizing RFC 3339 strings.

## Detection
Detected during manual testing of the `./dev-mock-data.sh` PITR workflow via `curl` against `/api/backups/quick-restore`, and diagnosed by inspecting `logs/node1.log` captured with `./dev-dump-logs.sh`.

## Resolution
- Updated `BackupService::quick_restore` in `crates/pgvisor-dashboard/src/handlers/backup.rs` to format `parsed_target` as `%Y-%m-%d %H:%M:%S UTC` before passing it to `restore_backup`.
- Added `to_postgres_timestamp` in `crates/pgvisor-sidecar/src/config.rs` and applied it in `ConfigGenerator::write_conf_only` so any RFC 3339 timestamp sent directly to the sidecar `/control/restore` API is converted to `%Y-%m-%d %H:%M:%S%.f+00` before being written to `postgresql.conf`.
- Added unit tests `test_quick_restore_formats_rfc3339_for_postgres` and `test_to_postgres_timestamp_converts_rfc3339`.

## Lessons Learned / Action Items
- [x] Normalize `recovery_target_time` in `BackupService::quick_restore` (`crates/pgvisor-dashboard/src/handlers/backup.rs`).
- [x] Normalize RFC 3339 timestamps at the `postgresql.conf` writer boundary (`crates/pgvisor-sidecar/src/config.rs::to_postgres_timestamp`).
- [x] Add an integration test case in `tests/test-incremental-pitr.sh` exercising `/api/backups/quick-restore` with an ISO-8601 `T...Z` timestamp against a live PostgreSQL instance.

