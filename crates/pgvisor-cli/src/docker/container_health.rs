/// Healthcheck status reported by `docker inspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerHealth {
    Healthy,
    Starting,
    Unhealthy,
    None,
    Unknown,
}

impl ContainerHealth {
    /// Parses a trimmed `docker inspect` health status string.
    pub fn parse_output(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "healthy" => Self::Healthy,
            "starting" => Self::Starting,
            "unhealthy" => Self::Unhealthy,
            "" | "none" | "<no value>" => Self::None,
            _ => Self::Unknown,
        }
    }

    /// Returns a human-readable lowercase label for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Starting => "starting",
            Self::Unhealthy => "unhealthy",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}
