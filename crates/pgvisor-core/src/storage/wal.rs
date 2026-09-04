use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher;
use openraft::{Entry, LogId};
use tracing::{info, warn};

use crate::error::StorageError;
use crate::raft::types::TypeConfig;

pub const WAL_MAGIC: [u8; 4] = *b"PGV1";
pub const HEADER_LEN: usize = 12; // 4 bytes magic + 4 bytes len + 4 bytes crc32

#[derive(Debug, Clone, Copy)]
pub struct LogIndexEntry {
    pub file_offset: u64,
    pub total_record_len: u64,
    pub log_id: LogId<u64>,
}

/// Pure-Rust append-only Write-Ahead Log (WAL) for OpenRaft entries.
///
/// Layout per record on disk:
/// - 4 bytes: Magic `PGV1`
/// - 4 bytes: Big-endian payload length `N`
/// - 4 bytes: Big-endian CRC32 checksum over payload
/// - `N` bytes: Bincode-serialized `Entry<TypeConfig>`
pub struct Wal {
    file: File,
    file_path: PathBuf,
    index: BTreeMap<u64, LogIndexEntry>,
    last_log_id: Option<LogId<u64>>,
}

impl Wal {
    /// Opens an existing WAL or initializes a new one at the given file path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let file_path = path.as_ref().to_path_buf();
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&file_path)?;

        let mut index = BTreeMap::new();
        let mut last_log_id = None;
        let mut current_offset: u64 = 0;

        let file_len = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;

        // Scan the WAL sequentially to build the in-memory index and verify data integrity.
        while current_offset < file_len {
            let mut header_buf = [0u8; HEADER_LEN];
            match file.read_exact(&mut header_buf) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    warn!(
                        offset = current_offset,
                        "Incomplete record header encountered, truncating WAL"
                    );
                    file.set_len(current_offset)?;
                    file.sync_all()?;
                    break;
                }
                Err(err) => return Err(StorageError::Io(err)),
            }

            let mut magic = [0u8; 4];
            magic.copy_from_slice(&header_buf[0..4]);
            if magic != WAL_MAGIC {
                warn!(
                    offset = current_offset,
                    "Corrupt magic bytes in WAL, truncating trailing data"
                );
                file.set_len(current_offset)?;
                file.sync_all()?;
                break;
            }

            let payload_len = (&header_buf[4..8]).read_u32::<BigEndian>()? as usize;
            let expected_crc = (&header_buf[8..12]).read_u32::<BigEndian>()?;

            let mut payload = vec![0u8; payload_len];
            match file.read_exact(&mut payload) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    warn!(
                        offset = current_offset,
                        "Truncated record payload in WAL, truncating to last clean offset"
                    );
                    file.set_len(current_offset)?;
                    file.sync_all()?;
                    break;
                }
                Err(err) => return Err(StorageError::Io(err)),
            }

            let mut hasher = Hasher::new();
            hasher.update(&payload);
            let calculated_crc = hasher.finalize();

            if calculated_crc != expected_crc {
                warn!(
                    offset = current_offset,
                    expected_crc = expected_crc,
                    calculated_crc = calculated_crc,
                    "Checksum mismatch in WAL record, truncating from this offset"
                );
                file.set_len(current_offset)?;
                file.sync_all()?;
                break;
            }

            let entry: Entry<TypeConfig> = bincode::deserialize(&payload)?;
            let total_record_len = (HEADER_LEN + payload_len) as u64;
            let log_id = entry.log_id;

            index.insert(
                log_id.index,
                LogIndexEntry {
                    file_offset: current_offset,
                    total_record_len,
                    log_id,
                },
            );

            last_log_id = Some(log_id);
            current_offset += total_record_len;
        }

        file.seek(SeekFrom::End(0))?;

        info!(
            recovered_entries = index.len(),
            last_log_id = ?last_log_id,
            "WAL initialized and validated"
        );

        Ok(Self {
            file,
            file_path,
            index,
            last_log_id,
        })
    }

    /// Appends a slice of Raft log entries sequentially to disk and flushes.
    pub fn append(&mut self, entries: &[Entry<TypeConfig>]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }

        self.file.seek(SeekFrom::End(0))?;
        let mut current_offset = self.file.stream_position()?;

        for entry in entries {
            let payload = bincode::serialize(entry)?;
            let payload_len = payload.len() as u32;

            let mut hasher = Hasher::new();
            hasher.update(&payload);
            let crc = hasher.finalize();

            self.file.write_all(&WAL_MAGIC)?;
            self.file.write_u32::<BigEndian>(payload_len)?;
            self.file.write_u32::<BigEndian>(crc)?;
            self.file.write_all(&payload)?;

            let total_len = (HEADER_LEN + payload.len()) as u64;
            self.index.insert(
                entry.log_id.index,
                LogIndexEntry {
                    file_offset: current_offset,
                    total_record_len: total_len,
                    log_id: entry.log_id,
                },
            );

            self.last_log_id = Some(entry.log_id);
            current_offset += total_len;
        }

        self.file.sync_all()?;
        Ok(())
    }

    /// Reads a single entry by its log index.
    pub fn read_entry(&mut self, index: u64) -> Result<Option<Entry<TypeConfig>>, StorageError> {
        let entry_meta = match self.index.get(&index) {
            Some(entry) => *entry,
            None => return Ok(None),
        };

        let payload_offset = entry_meta.file_offset + HEADER_LEN as u64;
        let payload_len = (entry_meta.total_record_len - HEADER_LEN as u64) as usize;

        self.file.seek(SeekFrom::Start(payload_offset))?;
        let mut payload = vec![0u8; payload_len];
        self.file.read_exact(&mut payload)?;

        let entry: Entry<TypeConfig> = bincode::deserialize(&payload)?;
        Ok(Some(entry))
    }

    /// Reads an inclusive-exclusive range of entries `[start, end)`.
    pub fn read_range(
        &mut self,
        start: u64,
        end: u64,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError> {
        let mut entries = Vec::new();
        for index in start..end {
            if let Some(entry) = self.read_entry(index)? {
                entries.push(entry);
            } else {
                break;
            }
        }
        Ok(entries)
    }

    /// Truncates the WAL, removing all entries starting from `since_index` onward.
    pub fn truncate_since(&mut self, since_index: u64) -> Result<(), StorageError> {
        if let Some((&first_key, entry_meta)) = self.index.range(since_index..).next() {
            let truncate_offset = entry_meta.file_offset;
            self.file.set_len(truncate_offset)?;
            self.file.sync_all()?;
            self.file.seek(SeekFrom::End(0))?;

            self.index.split_off(&first_key);
            self.last_log_id = self.index.values().next_back().map(|e| e.log_id);

            info!(
                since_index,
                new_last_log_id = ?self.last_log_id,
                "WAL truncated successfully"
            );
        }
        Ok(())
    }

    /// Purges index entries strictly before `upto_index`.
    pub fn purge_upto(&mut self, upto_index: u64) {
        let keys_to_remove: Vec<u64> = self
            .index
            .range(..=upto_index)
            .map(|(&k, _)| k)
            .collect();

        for k in keys_to_remove {
            self.index.remove(&k);
        }
    }

    /// Returns the last log ID recorded in the WAL.
    pub fn last_log_id(&self) -> Option<LogId<u64>> {
        self.last_log_id
    }

    /// Returns the first log ID available in the active index.
    pub fn first_log_id(&self) -> Option<LogId<u64>> {
        self.index.values().next().map(|e| e.log_id)
    }

    /// Returns the filesystem path to the WAL file.
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    /// Returns the number of entries indexed.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns true if no entries are indexed.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}
