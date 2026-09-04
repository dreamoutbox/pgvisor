use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openraft::storage::{
    LogFlushed, LogState, RaftLogReader, RaftLogStorage, RaftSnapshotBuilder, RaftStateMachine,
};
use openraft::{
    AnyError, Entry, EntryPayload, ErrorSubject, ErrorVerb, LogId, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership, Vote,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::raft::types::{ClusterResponse, NodeInfo, TypeConfig};
use crate::storage::state_machine::StateMachine;
use crate::storage::wal::Wal;

/// Persistent metadata holding the Raft vote and last purged log ID.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LogMetadata {
    vote: Option<Vote<u64>>,
    last_purged_log_id: Option<LogId<u64>>,
}

/// Core log storage state guarded by a mutex.
struct LogStoreInner {
    dir_path: PathBuf,
    meta_path: PathBuf,
    meta: LogMetadata,
    wal: Wal,
}

impl LogStoreInner {
    fn open(dir: impl AsRef<Path>) -> Result<Self, crate::error::StorageError> {
        let dir_path = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir_path)?;

        let wal_path = dir_path.join("wal.bin");
        let wal = Wal::open(&wal_path)?;

        let meta_path = dir_path.join("meta.json");
        let meta = if meta_path.exists() {
            let mut file = File::open(&meta_path)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            LogMetadata::default()
        };

        Ok(Self {
            dir_path,
            meta_path,
            meta,
            wal,
        })
    }

    fn persist_meta(&self) -> Result<(), crate::error::StorageError> {
        let payload = serde_json::to_vec_pretty(&self.meta)?;
        let tmp = self.meta_path.with_extension("tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.meta_path)?;
        Ok(())
    }
}

/// Custom pure-Rust OpenRaft LogStore.
#[derive(Clone)]
pub struct LogStore {
    inner: Arc<Mutex<LogStoreInner>>,
}

impl LogStore {
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, crate::error::StorageError> {
        let inner = LogStoreInner::open(dir)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }
}

/// Reader for Raft log entries.
#[derive(Clone)]
pub struct LogReader {
    inner: Arc<Mutex<LogStoreInner>>,
}

impl RaftLogReader<TypeConfig> for LogReader {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<u64>> {
        let mut inner = self.inner.lock().await;

        let start = match range.start_bound() {
            std::ops::Bound::Included(&n) => n,
            std::ops::Bound::Excluded(&n) => n + 1,
            std::ops::Bound::Unbounded => {
                inner.meta.last_purged_log_id.map(|l| l.index + 1).unwrap_or(0)
            }
        };

        let end = match range.end_bound() {
            std::ops::Bound::Included(&n) => n + 1,
            std::ops::Bound::Excluded(&n) => n,
            std::ops::Bound::Unbounded => {
                inner.wal.last_log_id().map(|l| l.index + 1).unwrap_or(start)
            }
        };

        let entries = inner.wal.read_range(start, end).map_err(|err| {
            StorageIOError::new(ErrorSubject::Logs, ErrorVerb::Read, AnyError::new(&err))
        })?;

        Ok(entries)
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<u64>> {
        let mut reader = self.get_log_reader().await;
        reader.try_get_log_entries(range).await
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = LogReader;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<u64>> {
        let inner = self.inner.lock().await;
        Ok(LogState {
            last_purged_log_id: inner.meta.last_purged_log_id,
            last_log_id: inner.wal.last_log_id(),
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        LogReader {
            inner: Arc::clone(&self.inner),
        }
    }

    async fn save_vote(&mut self, vote: &Vote<u64>) -> Result<(), StorageError<u64>> {
        let mut inner = self.inner.lock().await;
        inner.meta.vote = Some(*vote);
        inner.persist_meta().map_err(|err| {
            StorageIOError::new(ErrorSubject::Vote, ErrorVerb::Write, AnyError::new(&err))
        })?;
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<u64>>, StorageError<u64>> {
        let inner = self.inner.lock().await;
        Ok(inner.meta.vote)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
    {
        let entries_vec: Vec<Entry<TypeConfig>> = entries.into_iter().collect();
        let result = {
            let mut inner = self.inner.lock().await;
            inner.wal.append(&entries_vec)
        };

        match result {
            Ok(()) => {
                callback.log_io_completed(Ok(()));
                Ok(())
            }
            Err(err) => {
                let io_err = std::io::Error::new(std::io::ErrorKind::Other, err.to_string());
                callback.log_io_completed(Err(io_err));
                Err(StorageIOError::new(
                    ErrorSubject::Logs,
                    ErrorVerb::Write,
                    AnyError::new(&err),
                )
                .into())
            }
        }
    }

    async fn truncate(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut inner = self.inner.lock().await;
        inner.wal.truncate_since(log_id.index).map_err(|err| {
            StorageIOError::new(ErrorSubject::Logs, ErrorVerb::Write, AnyError::new(&err))
        })?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut inner = self.inner.lock().await;
        inner.meta.last_purged_log_id = Some(log_id);
        inner.wal.purge_upto(log_id.index);
        inner.persist_meta().map_err(|err| {
            StorageIOError::new(ErrorSubject::Logs, ErrorVerb::Write, AnyError::new(&err))
        })?;
        Ok(())
    }
}

/// Custom pure-Rust OpenRaft State Machine store.
#[derive(Clone)]
pub struct StateMachineStore {
    inner: Arc<Mutex<StateMachine>>,
}

impl StateMachineStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, crate::error::StorageError> {
        let sm = StateMachine::open(path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(sm)),
        })
    }
}

/// Snapshot builder producing serializable in-memory snapshots.
pub struct SnapshotBuilder {
    snapshot_data: Vec<u8>,
    last_applied: Option<LogId<u64>>,
    last_membership: StoredMembership<u64, NodeInfo>,
}

impl RaftSnapshotBuilder<TypeConfig> for SnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<u64>> {
        let last_applied = self.last_applied.unwrap_or_default();
        let meta = SnapshotMeta {
            last_log_id: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot_id: format!("{}-{}", last_applied.leader_id, last_applied.index),
        };
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(self.snapshot_data.clone())),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachineStore {
    type SnapshotBuilder = SnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, NodeInfo>), StorageError<u64>> {
        let sm = self.inner.lock().await;
        Ok((sm.data.last_applied, sm.data.last_membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<ClusterResponse>, StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
    {
        let mut sm = self.inner.lock().await;
        let mut responses = Vec::new();

        for entry in entries {
            sm.data.last_applied = Some(entry.log_id);

            match entry.payload {
                EntryPayload::Normal(cmd) => {
                    let resp = sm.apply_command(cmd);
                    responses.push(resp);
                }
                EntryPayload::Membership(mem) => {
                    sm.data.last_membership = StoredMembership::new(Some(entry.log_id), mem);
                    responses.push(ClusterResponse::Success);
                }
                EntryPayload::Blank => {
                    responses.push(ClusterResponse::Success);
                }
            }
        }

        sm.persist_to_disk().map_err(|err| {
            StorageIOError::new(
                ErrorSubject::StateMachine,
                ErrorVerb::Write,
                AnyError::new(&err),
            )
        })?;

        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        let sm = self.inner.lock().await;
        let snapshot_data = sm.build_snapshot_payload().unwrap_or_default();
        SnapshotBuilder {
            snapshot_data,
            last_applied: sm.data.last_applied,
            last_membership: sm.data.last_membership.clone(),
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<u64>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<u64, NodeInfo>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<u64>> {
        let mut sm = self.inner.lock().await;
        let data = snapshot.into_inner();
        sm.restore_from_snapshot(&data).map_err(|err| {
            StorageIOError::new(
                ErrorSubject::Snapshot(Some(meta.signature())),
                ErrorVerb::Write,
                AnyError::new(&err),
            )
        })?;
        sm.data.last_applied = meta.last_log_id;
        sm.data.last_membership = meta.last_membership.clone();
        sm.persist_to_disk().map_err(|err| {
            StorageIOError::new(
                ErrorSubject::Snapshot(Some(meta.signature())),
                ErrorVerb::Write,
                AnyError::new(&err),
            )
        })?;
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<u64>> {
        let sm = self.inner.lock().await;
        let last_applied = match sm.data.last_applied {
            Some(id) => id,
            None => return Ok(None),
        };

        let data = sm.build_snapshot_payload().map_err(|err| {
            StorageIOError::new(
                ErrorSubject::StateMachine,
                ErrorVerb::Read,
                AnyError::new(&err),
            )
        })?;

        let meta = SnapshotMeta {
            last_log_id: Some(last_applied),
            last_membership: sm.data.last_membership.clone(),
            snapshot_id: format!("{}-{}", last_applied.leader_id, last_applied.index),
        };

        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::types::ClusterCommand;
    use openraft::{LeaderId, LogId};
    use tempfile::tempdir;

    fn make_test_entry(index: u64, key: &str, value: &str) -> Entry<TypeConfig> {
        Entry {
            log_id: LogId::new(LeaderId::new(1, 1), index),
            payload: EntryPayload::Normal(ClusterCommand::SetState {
                key: key.to_string(),
                value: value.to_string(),
            }),
        }
    }

    #[tokio::test]
    async fn test_wal_append_and_recovery() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("wal.bin");

        // 1. Append entries
        {
            let mut wal = Wal::open(&wal_path).unwrap();
            let e1 = make_test_entry(1, "k1", "v1");
            let e2 = make_test_entry(2, "k2", "v2");
            wal.append(&[e1, e2]).unwrap();
            assert_eq!(wal.len(), 2);
            assert_eq!(wal.last_log_id().unwrap().index, 2);
        }

        // 2. Reopen and verify crash recovery
        {
            let mut wal = Wal::open(&wal_path).unwrap();
            assert_eq!(wal.len(), 2);
            assert_eq!(wal.last_log_id().unwrap().index, 2);

            let read_e1 = wal.read_entry(1).unwrap().unwrap();
            assert_eq!(read_e1.log_id.index, 1);
            if let EntryPayload::Normal(ClusterCommand::SetState { key, value }) = read_e1.payload {
                assert_eq!(key, "k1");
                assert_eq!(value, "v1");
            } else {
                panic!("unexpected entry payload");
            }
        }
    }

    #[tokio::test]
    async fn test_wal_truncation() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("wal.bin");

        let mut wal = Wal::open(&wal_path).unwrap();
        let e1 = make_test_entry(1, "k1", "v1");
        let e2 = make_test_entry(2, "k2", "v2");
        let e3 = make_test_entry(3, "k3", "v3");
        wal.append(&[e1, e2, e3]).unwrap();
        assert_eq!(wal.len(), 3);

        // Truncate from index 2
        wal.truncate_since(2).unwrap();
        assert_eq!(wal.len(), 1);
        assert_eq!(wal.last_log_id().unwrap().index, 1);

        // Read index 2 should return None
        assert!(wal.read_entry(2).unwrap().is_none());
    }

    #[tokio::test]
    async fn test_state_machine_apply_and_snapshot() {
        let dir = tempdir().unwrap();
        let sm_path = dir.path().join("state_machine.bin");

        let mut sm = StateMachine::open(&sm_path).unwrap();
        let resp = sm.apply_command(ClusterCommand::SetState {
            key: "cluster_name".into(),
            value: "pgvisor_prod".into(),
        });
        assert_eq!(resp, ClusterResponse::Success);
        sm.data.last_applied = Some(LogId::new(LeaderId::new(1, 1), 1));
        sm.persist_to_disk().unwrap();

        // Verify snapshot build and restore
        let snapshot = sm.build_snapshot_payload().unwrap();

        let restore_path = dir.path().join("sm_restored.bin");
        let mut sm2 = StateMachine::open(&restore_path).unwrap();
        sm2.restore_from_snapshot(&snapshot).unwrap();

        assert_eq!(
            sm2.data.kvs.get("cluster_name").map(|s| s.as_str()),
            Some("pgvisor_prod")
        );
        assert_eq!(sm2.data.last_applied.unwrap().index, 1);
    }
}
