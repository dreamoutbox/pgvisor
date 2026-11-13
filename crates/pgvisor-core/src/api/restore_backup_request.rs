use serde::{Deserialize, Serialize};

use crate::backup::RecoveryTarget;

/// Request to restore cluster from a specific snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreBackupRequest {
    #[serde(default)]
    pub recovery_target_time: Option<String>,
    #[serde(default)]
    pub recovery_target_xid: Option<String>,
    #[serde(default)]
    pub recovery_target: Option<RecoveryTarget>,
}

impl RestoreBackupRequest {
    /// Resolves the intended recovery target specification, giving precedence to explicit
    /// target enum values followed by XID and time strings.
    pub fn target(&self) -> Option<RecoveryTarget> {
        if let Some(target) = self.recovery_target.as_ref() {
            Some(target.clone())
        } else if let Some(xid) = self.recovery_target_xid.as_ref() {
            let trimmed = xid.trim();
            if !trimmed.is_empty() {
                Some(RecoveryTarget::Xid(trimmed.to_string()))
            } else {
                None
            }
        } else if let Some(time) = self.recovery_target_time.as_ref() {
            let trimmed = time.trim();
            if !trimmed.is_empty() {
                Some(RecoveryTarget::Time(trimmed.to_string()))
            } else {
                None
            }
        } else {
            None
        }
    }
}
