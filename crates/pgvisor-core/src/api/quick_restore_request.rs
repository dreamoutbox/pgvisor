use serde::{Deserialize, Serialize};

/// Request to perform a quick restore by specifying only the target time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickRestoreRequest {
    pub recovery_target_time: String,
}
