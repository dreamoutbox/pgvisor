---
id: "005"
title: "OpenDAL WAL Archiving and Basebackup Pipeline"
type: "research"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How does PostgreSQL's `archive_command` / `restore_command` interface with the sidecar's OpenDAL worker (e.g., local CLI helper vs Unix domain socket IPC), how are `pg_basebackup` snapshots triggered/streamed, and how are snapshot metadata and WAL retention policies structured?

## Resolution

Implemented continuous WAL archiving, basebackup streaming, and retention pruning using OpenDAL in `crates/pgvisor-core/src/backup/manager.rs`:
1. **Object Storage Hierarchy**:
   - WAL segments: `clusters/<cluster_name>/wal/<file_name>` (e.g., `000000010000000000000001`).
   - Basebackups: `clusters/<cluster_name>/basebackups/<snapshot_id>.tar.gz` and accompanied metadata `clusters/<cluster_name>/basebackups/<snapshot_id>.json`.
2. **WAL Archiving & Restore (`BackupManager`)**:
   - `archive_wal`: Directly uploads PostgreSQL 16MB WAL segments to OpenDAL. Invoked by `archive_command`.
   - `restore_wal`: Checks if WAL segment exists in OpenDAL; downloads it into local directory, returning `Ok(true)` if found or `Ok(false)` on missing segment (standard PostgreSQL recovery signal for reaching end of WAL archive).
3. **Physical Basebackup Snapshotting**:
   - `save_basebackup`: Persists streaming tarballs and companion metadata (`BasebackupMeta` with `start_wal`, `stop_wal`, `created_at`, `total_bytes`).
   - `list_basebackups`: Lists chronological snapshots.
4. **Point-In-Time-Recovery (PITR) & Retention Management**:
   - `prune_retention`: Deletes basebackup snapshots older than `keep_count`. Prunes all archived WAL segments older than the `start_wal` of the oldest retained basebackup, preventing unbounded storage growth while preserving PITR capability.
5. **Unit Tests**:
   - Verified WAL archiving and byte-for-byte restore using OpenDAL.
   - Verified basebackup saving, chronological listing, and retention pruning.
