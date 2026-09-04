use thiserror::Error;

/// Storage errors encountered during log persistence, state machine application, or recovery.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Corrupted record magic: expected {expected:?}, found {found:?}")]
    CorruptedMagic { expected: [u8; 4], found: [u8; 4] },

    #[error("CRC32 checksum mismatch at offset {offset}: expected {expected:#010x}, calculated {calculated:#010x}")]
    ChecksumMismatch {
        expected: u32,
        calculated: u32,
        offset: u64,
    },

    #[error("Corrupted log record: {0}")]
    CorruptedRecord(String),

    #[error("Log index {index} has been purged")]
    LogPurged { index: u64 },

    #[error("Internal concurrency lock poisoned")]
    LockPoisoned,
}
