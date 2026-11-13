use serde::{Deserialize, Serialize};

use crate::backup::BackupType;

/// Request to trigger a physical backup snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateBackupRequest {
    pub backup_type: Option<BackupType>,
    pub label: Option<String>,
}
