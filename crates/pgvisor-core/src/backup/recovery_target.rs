use serde::{Deserialize, Serialize};

/// Target specification for Point-In-Time Recovery (PITR) stop points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RecoveryTarget {
    /// Replay WAL forward up to a target timestamp (UTC string or RFC 3339).
    Time(String),
    /// Replay WAL forward up to a specific transaction identifier (XID).
    Xid(String),
    /// Replay all available archived WAL segments to the latest point in time.
    Latest,
}

impl RecoveryTarget {
    /// Returns the target timestamp if this target is time-based.
    ///
    /// # Example
    /// ```rust
    /// use pgvisor_core::backup::RecoveryTarget;
    ///
    /// let target = RecoveryTarget::Time("2026-09-14 03:00:00".to_string());
    /// assert_eq!(target.as_time_str(), Some("2026-09-14 03:00:00"));
    /// assert_eq!(target.as_xid_str(), None);
    /// ```
    pub fn as_time_str(&self) -> Option<&str> {
        match self {
            Self::Time(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the target transaction ID if this target is XID-based.
    ///
    /// # Example
    /// ```rust
    /// use pgvisor_core::backup::RecoveryTarget;
    ///
    /// let target = RecoveryTarget::Xid("10050".to_string());
    /// assert_eq!(target.as_xid_str(), Some("10050"));
    /// assert_eq!(target.as_time_str(), None);
    /// ```
    pub fn as_xid_str(&self) -> Option<&str> {
        match self {
            Self::Xid(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

