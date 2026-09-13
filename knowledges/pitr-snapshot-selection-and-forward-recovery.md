# PostgreSQL PITR Snapshot Selection & Forward Recovery Mechanics

## Overview

Point-In-Time-Recovery (PITR) allows rolling a PostgreSQL database back or forward to any microsecond timestamp in its transaction history. However, a common operator pitfall is selecting a basebackup snapshot that was taken **after** the desired recovery target time and wondering why subsequent committed transactions were not "undone".

This document explains the physical mechanics of PostgreSQL PITR, details why snapshots cannot roll backward, walks through a real-world demonstration scenario, and outlines the correct operational rules for snapshot selection.

---

## 1. The Core Principle: Forward-Only Recovery

In PostgreSQL, physical basebackups and continuous WAL archiving operate under fundamental physical constraints:

1. **Snapshots are Full Physical Copies**:
   A basebackup archive (whether labeled "Full" or "Incremental") contains the exact physical disk state (`PGDATA`) of tables, indexes, and control files at the time `pg_basebackup` ran.
2. **Restoring a Snapshot Overwrites Disk**:
   When a restore is initiated, the supervisor replaces `PGDATA` with the archive's contents. All transactions committed prior to that snapshot's checkpoint **already exist on disk**.
3. **WAL Replay Only Moves Forward**:
   PostgreSQL recovery processes WAL records sequentially forward in time, starting from the snapshot's checkpoint LSN (`redo LSN`) up to the specified `recovery_target_time`.
4. **PostgreSQL Cannot Roll Backward**:
   PostgreSQL has no mechanism to "reverse-play" WAL records or undo committed heap pages that are already present in the restored base files. If `recovery_target_time` is earlier than the snapshot's checkpoint time, PostgreSQL sees that the base state is already past the target time, skips replay, and finishes recovery immediately.

> **Golden Rule of PITR**:
> Always select a basebackup snapshot created **BEFORE** your desired target timestamp. PostgreSQL will restore the earlier baseline and replay WAL forward until it reaches the target time.

---

## 2. Case Study: The 7-Row Demo Incident

### Sequence of Events

During automated testing and manual UI exploration, the following sequence was executed:

```
T0: 19:03:02.89  [Snapshot f1 taken]       --> 4 rows (alpha, beta, gamma, delta)
T1: 19:03:08.24  INSERT 'echo'             --> 5 rows
T2: 19:03:11.31  INSERT 'foxtrot'          --> 6 rows
T3: 19:03:14.38  INSERT 'golf'             --> 7 rows (Tx 755 committed)
T4: 19:03:15.60  [Snapshot incr2 taken]    --> 7 rows (basebackup archive contains golf!)
```

### What Happened When Restoring `incr2` with Target `19:03:11.31`

The operator selected **`incr2`** (`snap-20260913-190315`) in the dashboard and entered `2026-09-13 19:03:11.31` (aiming to stop after `foxtrot` and exclude `golf`).

**Observed Result**: The restored database still had all 7 rows (including `golf`).

**PostgreSQL Server Log**:
```text
LOG: database system was interrupted; last known up at 2026-09-13 19:03:16 GMT
LOG: starting backup recovery with redo LSN 0/7000028, checkpoint LSN 0/7000080, on timeline ID 1
LOG: starting point-in-time recovery to 2026-09-13 19:03:11+00
LOG: completed backup recovery with redo LSN 0/7000028 and end LSN 0/7000120
LOG: consistent recovery state reached at 0/7000120
LOG: archive recovery complete
LOG: database system is ready to accept connections
```

**Why**:
- The `incr2` archive was created at `19:03:15.60`, after `golf` had already been committed at `19:03:14.38`.
- Extracting `incr2` restored data files that **already had row 7 (`golf`) inside them**.
- Because the recovery target (`19:03:11`) was earlier than the snapshot redo checkpoint (`19:03:16`), recovery reached a consistent state immediately and did not (and could not) remove `golf`.

---

## 3. The Correct Procedure: Restoring from `f1`

To recover to `2026-09-13 19:03:12` (retaining `alpha` through `foxtrot`, but excluding `golf`):

1. **Select Snapshot `f1`** (`snap-20260913-190302`), which was created at `19:03:02` (prior to `19:03:12`).
2. Supply `recovery_target_time = '2026-09-13 19:03:12'`.
3. PostgreSQL extracts `f1` (rows 1–4) and restores WAL segments from MinIO/S3.

**PostgreSQL Server Log During Successful Restore**:
```text
LOG: database system was interrupted; last known up at 2026-09-13 19:03:07 GMT
LOG: starting backup recovery with redo LSN 0/5000028, checkpoint LSN 0/5000080, on timeline ID 1
LOG: starting point-in-time recovery to 2026-09-13 19:03:12+00
LOG: redo starts at 0/5000028
LOG: restored log file "000000010000000000000006" from archive
LOG: restored log file "000000010000000000000007" from archive
LOG: recovery stopping before commit of transaction 755, time 2026-09-13 19:03:14.385873+00
LOG: last completed transaction was at log time 2026-09-13 19:03:11.317621+00
LOG: selected new timeline ID: 3
LOG: archive recovery complete
LOG: database system is ready to accept connections
```

**Result**:
- Transaction 754 (`foxtrot`, committed at `19:03:11.31`) was replayed.
- Transaction 755 (`golf`, committed at `19:03:14.38`) was stopped before commit.
- Exactly **6 rows** present in the database.

---

## 4. Operator Quick Reference

| Goal | Selected Snapshot | Target Timestamp | Outcome |
|---|---|---|---|
| Restore exact state when snapshot was taken | Target Snapshot | *(leave empty)* | Database opens at snapshot checkpoint. |
| Restore to specific timestamp $T$ | **Latest snapshot created before $T$** | $T$ | Database replays WAL from snapshot to $T$. |
| Restore using snapshot created *after* $T$ | *(Invalid configuration)* | $T$ | **No rollback occurs**; changes in snapshot remain. |

---

## 5. Architectural Safeguards & Future Improvements

To prevent operator error when selecting snapshots for PITR:

1. **Dashboard Validation**: When a user inputs a `recovery_target_time`, compare it against the snapshot's `created_at` timestamp. If `target_time < snapshot.created_at`, display a blocking warning:
   > *"Selected snapshot was taken at [T_snap], which is later than target time [T_target]. PostgreSQL cannot roll backward. Please select a snapshot taken before [T_target]."*
2. **Automatic Base Selection**: Allow operators to input a desired timestamp directly, with `pgvisor-proxy` automatically resolving the closest prior basebackup snapshot to restore from.
