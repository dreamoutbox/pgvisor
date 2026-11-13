use std::collections::HashSet;

pub const SUPERUSER_ENV: &str = "PGVISOR_SUPERUSER";

/// Allowlist of PostgreSQL roles permitted to log in and manage the cluster via `/api/*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperuserAllowlist {
    users: HashSet<String>,
}

impl SuperuserAllowlist {
    /// Reads `PGVISOR_SUPERUSER` from the environment, falling back to `default_postgres_user`.
    ///
    /// # Arguments
    /// * `default_postgres_user` - Fallback superuser name (e.g. `POSTGRES_USER` or `"postgres"`).
    pub fn from_env(default_postgres_user: &str) -> Self {
        let raw = std::env::var(SUPERUSER_ENV).ok();
        Self::parse(raw.as_deref(), default_postgres_user)
    }

    /// Parses a comma-separated `PGVISOR_SUPERUSER` string, trimming and deduplicating entries.
    /// Falls back to `[default_postgres_user]` when unset or empty.
    ///
    /// # Arguments
    /// * `raw` - Optional raw env string (e.g. `Some("postgres, bob")`).
    /// * `default_postgres_user` - Fallback role when `raw` produces no valid entries.
    pub fn parse(raw: Option<&str>, default_postgres_user: &str) -> Self {
        let mut users: HashSet<String> = raw
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();

        if users.is_empty() {
            let fallback = default_postgres_user.trim();
            if !fallback.is_empty() {
                users.insert(fallback.to_string());
            } else {
                users.insert("postgres".to_string());
            }
        }

        Self { users }
    }

    /// Returns `true` if `username` is listed in `PGVISOR_SUPERUSER`.
    pub fn is_allowed(&self, username: &str) -> bool {
        self.users.contains(username)
    }

    /// Returns the number of configured superusers.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Returns `true` if the allowlist has no entries.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }
}
