### If you want to modify OpenRaft consensus types, state machine, or custom storage engine, then check:

- `crates/pgvisor-core/src/storage/wal.rs` = pure-Rust append-only WAL writer/reader, binary framing format, in-memory offset index, and CRC32 verification
- `crates/pgvisor-core/src/storage/state_machine.rs` = cluster state machine data, node roles, atomic disk persistence, and snapshot serialization
- `crates/pgvisor-core/src/storage/engine.rs` = OpenRaft `RaftLogStorage`, `RaftLogReader`, `RaftStateMachine`, and `RaftSnapshotBuilder` trait implementations
- `crates/pgvisor-core/src/raft/types.rs` = OpenRaft `TypeConfig`, `ClusterCommand`, `ClusterResponse`, and `NodeRole` definitions
- `crates/pgvisor-core/src/error.rs` = `StorageError` domain error enum for I/O, checksum, and corruption handling
- `Cargo.toml` = workspace manifest with OpenRaft `storage-v2` feature and shared dependencies
