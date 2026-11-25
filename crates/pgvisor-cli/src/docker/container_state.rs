/// Lifecycle state reported by `docker inspect --format '{{.State.Status}}'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    Created,
    Restarting,
    Paused,
    Exited,
    Dead,
    Missing,
    Unknown,
}

impl ContainerState {
    /// Parses a trimmed `docker inspect` state string.
    pub fn parse_output(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "running" => Self::Running,
            "created" => Self::Created,
            "restarting" => Self::Restarting,
            "paused" => Self::Paused,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            "" | "missing" => Self::Missing,
            _ => Self::Unknown,
        }
    }

    /// Returns `true` if the container has terminated unexpectedly (`Exited` or `Dead`).
    pub fn is_terminal_failure(self) -> bool {
        matches!(self, Self::Exited | Self::Dead)
    }

    /// Returns a human-readable lowercase label for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Created => "created",
            Self::Restarting => "restarting",
            Self::Paused => "paused",
            Self::Exited => "exited",
            Self::Dead => "dead",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}
