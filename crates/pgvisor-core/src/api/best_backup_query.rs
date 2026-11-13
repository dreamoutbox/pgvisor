use serde::{Deserialize, Serialize};

/// Query parameters for finding the best backup for a given target timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BestBackupQuery {
    pub target_time: String,
}
