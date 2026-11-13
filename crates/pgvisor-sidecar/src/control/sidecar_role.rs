use serde::{Deserialize, Serialize};

/// Typed operational role of a sidecar node within the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarRole {
    Leader,
    Standby,
    Fenced,
}

impl SidecarRole {
    /// Parses a role string into [`SidecarRole`], defaulting unknown values to `Standby`.
    ///
    /// # Arguments
    /// * `raw` - Raw role string stored in legacy control state.
    pub fn from_role_str(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "leader" => Self::Leader,
            "fenced" => Self::Fenced,
            _ => Self::Standby,
        }
    }

    /// Returns the canonical lowercase role label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Standby => "standby",
            Self::Fenced => "fenced",
        }
    }
}

impl std::fmt::Display for SidecarRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
