use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Postgres data directory {0} does not exist")]
    MissingDataDir(PathBuf),
}

/// Converts an RFC 3339 timestamp (e.g. `2026-11-02T00:29:36Z`) into the
/// `YYYY-MM-DD HH:MM:SS[.f]+00` form accepted by Postgres `recovery_target_time`
/// (Postgres rejects the `T`/`Z` form). Non-RFC 3339 input is returned unchanged.
///
/// Example: `to_postgres_timestamp("2026-11-02T00:29:36Z") == "2026-11-02 00:29:36+00"`
pub fn to_postgres_timestamp(input: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(input.trim()) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S%.f+00")
            .to_string(),
        Err(_) => input.to_string(),
    }
}

/// PostgreSQL `log_statement` options.
///
/// Controls which SQL statements are logged by the PostgreSQL server:
/// - `None`: logging disabled (`none`)
/// - `Ddl`: logs data definition language statements such as CREATE, ALTER, DROP (`ddl`)
/// - `Mod`: logs all DDL statements plus data-modifying write statements (INSERT, UPDATE, DELETE, TRUNCATE, COPY FROM) (`mod`)
/// - `All`: logs all SQL statements including read-only queries like SELECT (`all`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogStatement {
    None,
    Ddl,
    #[default]
    Mod,
    All,
}

impl LogStatement {
    /// Parses a string representation into a `LogStatement` variant.
    ///
    /// Accepts common aliases like "write" for `Mod`, and "off"/"false"/"0" for `None`.
    ///
    /// # Arguments
    /// * `value` - Input string specifying the desired logging level.
    ///
    /// # Example
    /// ```rust
    /// use pgvisor_sidecar::config::LogStatement;
    ///
    /// assert_eq!(LogStatement::parse("mod"), Some(LogStatement::Mod));
    /// assert_eq!(LogStatement::parse("write"), Some(LogStatement::Mod));
    /// assert_eq!(LogStatement::parse("off"), Some(LogStatement::None));
    /// ```
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "none" | "off" | "false" | "0" => Some(Self::None),
            "ddl" => Some(Self::Ddl),
            "mod" | "write" => Some(Self::Mod),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// Returns the corresponding PostgreSQL configuration string literal.
    pub fn as_pg_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ddl => "ddl",
            Self::Mod => "mod",
            Self::All => "all",
        }
    }
}

/// Postgres instance configuration parameters.
#[derive(Debug, Clone)]
pub struct PostgresConfig {
    pub port: u16,
    pub max_connections: u32,
    pub shared_buffers: String,
    pub wal_keep_size: String,
    pub log_statement: LogStatement,
    pub archive_command: Option<String>,
    pub restore_command: Option<String>,
    pub primary_conninfo: Option<String>,
    pub recovery_target_time: Option<String>,
    pub recovery_target_xid: Option<String>,
    pub recovery_target_inclusive: Option<bool>,
    pub recovery_target_action: Option<String>,
    pub ssl: bool,
    pub ssl_cert_file: Option<String>,
    pub ssl_key_file: Option<String>,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            port: 5432,
            max_connections: 100,
            shared_buffers: "128MB".to_string(),
            wal_keep_size: "1GB".to_string(),
            log_statement: LogStatement::Mod,
            archive_command: None,
            restore_command: None,
            primary_conninfo: None,
            recovery_target_time: None,
            recovery_target_xid: None,
            recovery_target_inclusive: None,
            recovery_target_action: None,
            ssl: false,
            ssl_cert_file: None,
            ssl_key_file: None,
        }
    }
}

/// Configuration generator for PostgreSQL instance data directory.
pub struct ConfigGenerator;

impl ConfigGenerator {
    /// Writes `postgresql.conf`, `pg_hba.conf`, and manages `recovery.signal`/`standby.signal`.
    /// Call this before starting Postgres so that signal files are in the correct state.
    pub fn write_configs(
        data_dir: impl AsRef<Path>,
        config: &PostgresConfig,
    ) -> Result<(), ConfigError> {
        let dir = data_dir.as_ref();
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }

        let conf_path = Self::write_postgresql_conf(dir, config)?;
        Self::write_pg_hba(dir)?;

        // Targeted recovery indicator file — must be present before Postgres starts recovery.
        // Postgres removes it itself upon successful promote; the sidecar must not race it.
        let recovery_signal = dir.join("recovery.signal");
        if config.recovery_target_time.is_some() || config.recovery_target_xid.is_some() {
            File::create(&recovery_signal)?;
            info!("Created recovery.signal for targeted PITR recovery");
        } else if recovery_signal.exists() {
            let _ = fs::remove_file(&recovery_signal);
        }

        // Standby indicator file
        if let Some(primary_info) = &config.primary_conninfo {
            let standby_signal = dir.join("standby.signal");
            File::create(standby_signal)?;
            info!(primary_info, "Created standby.signal for replica instance");
        } else {
            let standby_signal = dir.join("standby.signal");
            if standby_signal.exists() {
                let _ = fs::remove_file(standby_signal);
            }
        }

        info!(path = ?conf_path, "PostgreSQL configurations written successfully");
        Ok(())
    }

    /// Writes only the `.conf` files without touching `recovery.signal` or `standby.signal`.
    /// Safe to call while Postgres is running (e.g. to clear PITR settings after promote),
    /// because Postgres's startup process owns and removes `recovery.signal` itself.
    pub fn write_conf_only(
        data_dir: impl AsRef<Path>,
        config: &PostgresConfig,
    ) -> Result<(), ConfigError> {
        let dir = data_dir.as_ref();
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }

        let conf_path = Self::write_postgresql_conf(dir, config)?;
        Self::write_pg_hba(dir)?;

        info!(path = ?conf_path, "PostgreSQL configurations written successfully");
        Ok(())
    }

    fn write_postgresql_conf(dir: &Path, config: &PostgresConfig) -> Result<PathBuf, ConfigError> {
        let conf_path = dir.join("postgresql.conf");
        let mut conf_content = format!(
            "# Generated by pgvisor-sidecar\n\
            listen_addresses = '*'\n\
            port = {port}\n\
            max_connections = {max_conn}\n\
            shared_buffers = {buffers}\n\
            wal_level = replica\n\
            max_wal_senders = 10\n\
            wal_keep_size = '{wal_size}'\n\
            hot_standby = on\n\
            track_commit_timestamp = on\n\
            log_statement = '{log_statement}'\n\
            log_line_prefix = '%m [%p] tx=%x: '\n\
            unix_socket_directories = '/tmp'\n",
            port = config.port,
            max_conn = config.max_connections,
            buffers = config.shared_buffers,
            wal_size = config.wal_keep_size,
            log_statement = config.log_statement.as_pg_str()
        );

        if let Some(archive_cmd) = &config.archive_command {
            conf_content.push_str(&format!(
                "archive_mode = on\n\
                archive_command = '{archive_cmd}'\n"
            ));
        }

        if let Some(restore_cmd) = &config.restore_command {
            // Standbys streaming from primary must NOT use restore_command pointing to
            // shared archive, preventing standby crashes from orphaned/divergent timeline history files.
            if config.primary_conninfo.is_none() {
                conf_content.push_str(&format!("restore_command = '{restore_cmd}'\n"));
            }
        }

        if let Some(target_time) = &config.recovery_target_time {
            let pg_time = to_postgres_timestamp(target_time);
            conf_content.push_str(&format!("recovery_target_time = '{pg_time}'\n"));
            conf_content.push_str("recovery_target_timeline = 'current'\n");
        } else if let Some(target_xid) = &config.recovery_target_xid {
            conf_content.push_str(&format!("recovery_target_xid = '{target_xid}'\n"));
            conf_content.push_str("recovery_target_timeline = 'current'\n");
            if let Some(inclusive) = config.recovery_target_inclusive {
                let inc_str = if inclusive { "on" } else { "off" };
                conf_content.push_str(&format!("recovery_target_inclusive = {inc_str}\n"));
            }
        }

        if let Some(target_action) = &config.recovery_target_action {
            conf_content.push_str(&format!("recovery_target_action = '{target_action}'\n"));
        }

        if config.ssl {
            conf_content.push_str("ssl = on\n");
            if let Some(cert_file) = &config.ssl_cert_file {
                conf_content.push_str(&format!("ssl_cert_file = '{cert_file}'\n"));
            }
            if let Some(key_file) = &config.ssl_key_file {
                conf_content.push_str(&format!("ssl_key_file = '{key_file}'\n"));
            }
        } else {
            conf_content.push_str("ssl = off\n");
        }

        if let Some(primary_info) = &config.primary_conninfo {
            conf_content.push_str(&format!("primary_conninfo = '{primary_info}'\n"));
        }

        let mut conf_file = File::create(&conf_path)?;
        conf_file.write_all(conf_content.as_bytes())?;
        conf_file.sync_all()?;
        Ok(conf_path)
    }

    fn write_pg_hba(dir: &Path) -> Result<(), ConfigError> {
        let hba_path = dir.join("pg_hba.conf");
        let hba_content = "\
            # Generated by pgvisor-sidecar\n\
            # TYPE  DATABASE        USER            ADDRESS                 METHOD\n\
            local   all             all                                     trust\n\
            host    all             all             127.0.0.1/32            trust\n\
            host    all             all             ::1/128                 trust\n\
            host    all             all             all                     trust\n\
            host    replication     all             all                     trust\n";

        let mut hba_file = File::create(&hba_path)?;
        hba_file.write_all(hba_content.as_bytes())?;
        hba_file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_write_primary_config() {
        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            port: 5432,
            archive_command: Some("test_archive %p %f".into()),
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("port = 5432"));
        assert!(conf.contains("archive_mode = on"));
        assert!(!dir.path().join("standby.signal").exists());
        assert!(!dir.path().join("recovery.signal").exists());
    }

    #[test]
    fn test_write_standby_config() {
        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            port: 5433,
            primary_conninfo: Some("host=127.0.0.1 port=5432 user=postgres".into()),
            restore_command: Some("pgvisor-sidecar restore %f %p".into()),
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("primary_conninfo = 'host=127.0.0.1 port=5432 user=postgres'"));
        assert!(!conf.contains("recovery_target_timeline = 'current'"));
        assert!(!conf.contains("restore_command"));
        assert!(dir.path().join("standby.signal").exists());
        assert!(!dir.path().join("recovery.signal").exists());
    }

    #[test]
    fn test_to_postgres_timestamp_converts_rfc3339() {
        assert_eq!(
            to_postgres_timestamp("2026-11-02T00:29:36Z"),
            "2026-11-02 00:29:36+00"
        );
        assert_eq!(
            to_postgres_timestamp("2026-11-02T07:29:36+07:00"),
            "2026-11-02 00:29:36+00"
        );
        assert_eq!(
            to_postgres_timestamp("2026-09-13 18:32:49"),
            "2026-09-13 18:32:49"
        );
    }

    #[test]
    fn test_write_pitr_recovery_config() {
        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            port: 5432,
            restore_command: Some("pgvisor-sidecar restore %f %p".into()),
            recovery_target_time: Some("2026-09-13 18:32:49".into()),
            recovery_target_action: Some("promote".into()),
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("restore_command = 'pgvisor-sidecar restore %f %p'"));
        assert!(conf.contains("recovery_target_time = '2026-09-13 18:32:49'"));
        assert!(conf.contains("recovery_target_timeline = 'current'"));
        assert!(conf.contains("recovery_target_action = 'promote'"));
        assert!(dir.path().join("recovery.signal").exists());
        assert!(!dir.path().join("standby.signal").exists());
    }

    #[test]
    fn test_write_pitr_xid_recovery_config() {
        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            port: 5432,
            restore_command: Some("pgvisor-sidecar restore %f %p".into()),
            recovery_target_xid: Some("10050".into()),
            recovery_target_inclusive: Some(true),
            recovery_target_action: Some("promote".into()),
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("restore_command = 'pgvisor-sidecar restore %f %p'"));
        assert!(conf.contains("recovery_target_xid = '10050'"));
        assert!(conf.contains("recovery_target_timeline = 'current'"));
        assert!(conf.contains("recovery_target_inclusive = on"));
        assert!(conf.contains("recovery_target_action = 'promote'"));
        assert!(dir.path().join("recovery.signal").exists());
        assert!(!dir.path().join("standby.signal").exists());
    }

    #[test]
    fn test_write_primary_cleans_stale_standby_signal() {
        let dir = tempdir().unwrap();
        // Simulate pre-existing standby.signal
        let standby_signal = dir.path().join("standby.signal");
        fs::File::create(&standby_signal).unwrap();
        assert!(standby_signal.exists());

        let config = PostgresConfig {
            port: 5432,
            primary_conninfo: None,
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        assert!(!standby_signal.exists());
    }

    #[test]
    fn test_write_ssl_config() {
        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            port: 5432,
            ssl: true,
            ssl_cert_file: Some("/data/server.crt".into()),
            ssl_key_file: Some("/data/server.key".into()),
            ..Default::default()
        };

        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("ssl = on"));
        assert!(conf.contains("ssl_cert_file = '/data/server.crt'"));
        assert!(conf.contains("ssl_key_file = '/data/server.key'"));
    }

    #[test]
    fn test_log_statement_parsing_and_generation() {
        assert_eq!(LogStatement::parse("none"), Some(LogStatement::None));
        assert_eq!(LogStatement::parse("off"), Some(LogStatement::None));
        assert_eq!(LogStatement::parse("ddl"), Some(LogStatement::Ddl));
        assert_eq!(LogStatement::parse("mod"), Some(LogStatement::Mod));
        assert_eq!(LogStatement::parse("write"), Some(LogStatement::Mod));
        assert_eq!(LogStatement::parse("all"), Some(LogStatement::All));
        assert_eq!(LogStatement::parse("invalid"), None);

        let dir = tempdir().unwrap();
        let config = PostgresConfig {
            log_statement: LogStatement::Mod,
            ..Default::default()
        };
        ConfigGenerator::write_configs(dir.path(), &config).unwrap();
        let conf = fs::read_to_string(dir.path().join("postgresql.conf")).unwrap();
        assert!(conf.contains("log_statement = 'mod'"));
        assert!(conf.contains("log_line_prefix = '%m [%p] tx=%x: '"));
    }
}
