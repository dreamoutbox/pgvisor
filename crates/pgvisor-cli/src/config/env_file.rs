use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::error::CliError;

/// Pure-Rust `.env` key-value parser (no shell execution).
#[derive(Debug, Default, Clone)]
pub struct EnvFile {
    values: HashMap<String, String>,
}

impl EnvFile {
    /// Loads `<dir>/.env` if present, returning an empty `EnvFile` if it does not exist.
    pub fn load_optional(path: &Path) -> Result<Self, CliError> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path).map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&content, path)
    }

    /// Parses `.env` text into an `EnvFile`.
    pub fn parse(content: &str, path: &Path) -> Result<Self, CliError> {
        let mut values = HashMap::new();
        for (idx, raw_line) in content.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let normalized = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
            let Some((raw_key, raw_val)) = normalized.split_once('=') else {
                return Err(CliError::InvalidEnvFile {
                    path: path.to_path_buf(),
                    line_number: idx + 1,
                    reason: "expected KEY=VALUE".to_string(),
                });
            };
            let key = raw_key.trim();
            if key.is_empty() {
                return Err(CliError::InvalidEnvFile {
                    path: path.to_path_buf(),
                    line_number: idx + 1,
                    reason: "empty variable name".to_string(),
                });
            }
            let val = Self::unquote(raw_val.trim());
            values.insert(key.to_string(), val);
        }
        Ok(Self { values })
    }

    fn unquote(raw: &str) -> String {
        if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            raw[1..raw.len() - 1].to_string()
        } else {
            raw.to_string()
        }
    }

    /// Returns the value for `key` if present, or `default` otherwise.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values.get(key).map(String::as_str).unwrap_or(default)
    }

    /// Returns the value for `key` if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}
