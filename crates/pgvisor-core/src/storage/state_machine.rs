use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher;
use openraft::{LogId, StoredMembership};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::StorageError;
use crate::raft::types::{ClusterCommand, ClusterResponse, NodeInfo, NodeRole};

const STATE_MAGIC: [u8; 4] = *b"PGSM";

/// Serializable snapshot representation of the PgVisor state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachineData {
    pub last_applied: Option<LogId<u64>>,
    pub last_membership: StoredMembership<u64, NodeInfo>,
    pub kvs: BTreeMap<String, String>,
    pub nodes: BTreeMap<u64, NodeInfo>,
    pub roles: BTreeMap<u64, NodeRole>,
}

impl Default for StateMachineData {
    fn default() -> Self {
        Self {
            last_applied: None,
            last_membership: StoredMembership::default(),
            kvs: BTreeMap::new(),
            nodes: BTreeMap::new(),
            roles: BTreeMap::new(),
        }
    }
}

/// Persistent State Machine for OpenRaft.
pub struct StateMachine {
    file_path: PathBuf,
    pub data: StateMachineData,
}

impl StateMachine {
    /// Opens or initializes the state machine at the specified file path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let file_path = path.as_ref().to_path_buf();
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if !file_path.exists() {
            let sm = Self {
                file_path,
                data: StateMachineData::default(),
            };
            sm.persist_to_disk()?;
            return Ok(sm);
        }

        let mut file = File::open(&file_path)?;
        let mut magic = [0u8; 4];
        if let Err(err) = file.read_exact(&mut magic) {
            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                warn!("State machine file empty, initializing new state");
                let sm = Self {
                    file_path,
                    data: StateMachineData::default(),
                };
                sm.persist_to_disk()?;
                return Ok(sm);
            }
            return Err(StorageError::Io(err));
        }

        if magic != STATE_MAGIC {
            return Err(StorageError::CorruptedMagic {
                expected: STATE_MAGIC,
                found: magic,
            });
        }

        let expected_crc = file.read_u32::<BigEndian>()?;
        let mut payload = Vec::new();
        file.read_to_end(&mut payload)?;

        let mut hasher = Hasher::new();
        hasher.update(&payload);
        let calculated_crc = hasher.finalize();

        if calculated_crc != expected_crc {
            return Err(StorageError::ChecksumMismatch {
                expected: expected_crc,
                calculated: calculated_crc,
                offset: 8,
            });
        }

        let data: StateMachineData = bincode::deserialize(&payload)?;
        info!(
            last_applied = ?data.last_applied,
            nodes = data.nodes.len(),
            "State machine recovered from disk"
        );

        Ok(Self { file_path, data })
    }

    /// Atomically persists the current state machine data to disk with CRC32 checksum.
    pub fn persist_to_disk(&self) -> Result<(), StorageError> {
        let payload = bincode::serialize(&self.data)?;
        let mut hasher = Hasher::new();
        hasher.update(&payload);
        let crc = hasher.finalize();

        let temp_path = self.file_path.with_extension("tmp");
        {
            let mut file = File::create(&temp_path)?;
            file.write_all(&STATE_MAGIC)?;
            file.write_u32::<BigEndian>(crc)?;
            file.write_all(&payload)?;
            file.sync_all()?;
        }

        fs::rename(&temp_path, &self.file_path)?;
        Ok(())
    }

    /// Applies a single cluster command to the in-memory state.
    pub fn apply_command(&mut self, cmd: ClusterCommand) -> ClusterResponse {
        match cmd {
            ClusterCommand::RegisterNode { node_id, info } => {
                self.data.nodes.insert(node_id, info);
                self.data.roles.insert(node_id, NodeRole::Standby);
                ClusterResponse::Success
            }
            ClusterCommand::UpdateNodeRole { node_id, role } => {
                self.data.roles.insert(node_id, role);
                ClusterResponse::Success
            }
            ClusterCommand::SetState { key, value } => {
                self.data.kvs.insert(key, value);
                ClusterResponse::Success
            }
            ClusterCommand::DeleteState { key } => {
                let prev = self.data.kvs.remove(&key);
                ClusterResponse::Value(prev)
            }
        }
    }

    /// Serializes entire state into a snapshot payload.
    pub fn build_snapshot_payload(&self) -> Result<Vec<u8>, StorageError> {
        Ok(bincode::serialize(&self.data)?)
    }

    /// Restores state machine from a snapshot payload.
    pub fn restore_from_snapshot(&mut self, payload: &[u8]) -> Result<(), StorageError> {
        let data: StateMachineData = bincode::deserialize(payload)?;
        self.data = data;
        self.persist_to_disk()?;
        info!(
            last_applied = ?self.data.last_applied,
            "State machine restored from snapshot"
        );
        Ok(())
    }
}
