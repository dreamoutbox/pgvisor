---
id: "001"
title: "Custom OpenRaft Storage Engine Design"
type: "prototype"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

What is the exact data layout, on-disk file format (e.g., append-only log record format with index headers, state machine key-value store, and snapshot serialization), and concurrency model for our custom pure-Rust OpenRaft storage engine?

## Resolution

Implemented pure-Rust append-only storage engine in `crates/pgvisor-core/src/storage/`:
1. **WAL Framing Format (`wal.bin`)**:
   - 12-byte header: `MAGIC [4B: 'PGV1'] + PAYLOAD_LEN [4B BE] + CRC32 [4B BE]`.
   - Payload: `bincode`-serialized `openraft::Entry<TypeConfig>`.
   - Integrity: Checked on every read; trailing partial or corrupt writes from power loss are detected and automatically truncated to the last clean record offset upon startup.
2. **In-Memory Indexing**:
   - `BTreeMap<u64, LogIndexEntry>` mapping `log_index -> (file_offset, total_len, log_id)`.
   - Delivers $O(\log N)$ random seeks without linear file scanning.
3. **Persistent State Machine (`state_machine.bin`)**:
   - Tracks node registration, roles (`Leader`, `Standby`, `Fenced`), KV state, and `last_applied` LogId.
   - Atomic disk persistence via temporary file swap with CRC32 header.
   - Snapshot builder and installer supporting streaming state restore.
4. **OpenRaft 0.9 Trait Integration**:
   - `LogStore` implements `openraft::storage::RaftLogStorage<TypeConfig>`.
   - `LogReader` implements `openraft::storage::RaftLogReader<TypeConfig>`.
   - `StateMachineStore` implements `openraft::storage::RaftStateMachine<TypeConfig>`.
5. **Unit Tests**:
   - Added unit test suite in `crates/pgvisor-core/src/storage/engine.rs` proving sequential append, crash recovery scanning from disk, log truncation on conflict, and state machine snapshotting.
