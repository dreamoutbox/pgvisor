use std::io::{self, BufRead, IsTerminal, Write};

use crate::cli::{S3Mode, SetupArgs};
use crate::error::CliError;

/// Resolved configuration options for `pgvisor setup`.
#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub cluster_id: String,
    pub image: String,
    pub proxy_port: u16,
    pub dashboard_bind: String,
    pub dashboard_port: u16,
    pub superuser: String,
    pub s3_mode: S3Mode,
    pub s3_api_port: u16,
    pub s3_console_port: u16,
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub s3_region: String,
    pub s3_access_key: Option<String>,
    pub s3_secret_key: Option<String>,
    pub tls: bool,
    pub force: bool,
    pub start: bool,
}

impl SetupOptions {
    /// Resolves `SetupOptions` from CLI flags, prompting interactively on a TTY when allowed.
    pub fn resolve(args: &SetupArgs) -> Result<Self, CliError> {
        let interactive = !args.non_interactive && io::stdin().is_terminal();
        let mut stdin = io::stdin().lock();
        let mut stdout = io::stdout();

        let cluster_id = match args.cluster_id.as_ref() {
            Some(v) => v.clone(),
            None if interactive => Self::prompt_str(&mut stdin, &mut stdout, "Cluster ID", "pgvisor-cluster")?,
            None => "pgvisor-cluster".to_string(),
        };

        let image = match args.image.as_ref() {
            Some(v) => v.clone(),
            None => "dreamoutbox/pgvisor:latest".to_string(),
        };

        let proxy_port = match args.proxy_port {
            Some(p) => p,
            None if interactive => Self::prompt_u16(&mut stdin, &mut stdout, "PostgreSQL Proxy Port", 5432)?,
            None => 5432,
        };

        let dashboard_port = match args.dashboard_port {
            Some(p) => p,
            None if interactive => Self::prompt_u16(&mut stdin, &mut stdout, "Web Dashboard Port", 8080)?,
            None => 8080,
        };

        let superuser = match args.superuser.as_ref() {
            Some(v) => v.clone(),
            None => "postgres".to_string(),
        };

        let s3_mode = match args.s3_mode {
            Some(m) => m,
            None if interactive => {
                let raw = Self::prompt_str(&mut stdin, &mut stdout, "S3 Mode (bundled/external)", "bundled")?;
                match raw.trim().to_ascii_lowercase().as_str() {
                    "bundled" => S3Mode::Bundled,
                    "external" => S3Mode::External,
                    other => return Err(CliError::InvalidOption(format!("Unknown S3 mode '{other}' (expected 'bundled' or 'external')"))),
                }
            }
            None => S3Mode::Bundled,
        };

        let s3_api_port = args.s3_api_port.unwrap_or(9000);
        let s3_console_port = args.s3_console_port.unwrap_or(9001);

        let s3_bucket = match args.s3_bucket.as_ref() {
            Some(v) => v.clone(),
            None if interactive && s3_mode == S3Mode::External => {
                Self::prompt_str(&mut stdin, &mut stdout, "External S3 Bucket", "pgvisor-backups")?
            }
            None => "pgvisor-backups".to_string(),
        };

        let s3_region = args
            .s3_region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());

        let (s3_endpoint, s3_access_key, s3_secret_key) = match s3_mode {
            S3Mode::Bundled => (
                "http://rustfs:9000".to_string(),
                args.s3_access_key.clone(),
                args.s3_secret_key.clone(),
            ),
            S3Mode::External => {
                let endpoint = match args.s3_endpoint.as_ref() {
                    Some(v) if !v.trim().is_empty() => v.clone(),
                    _ if interactive => Self::prompt_required(&mut stdin, &mut stdout, "External S3 Endpoint URL")?,
                    _ => {
                        return Err(CliError::InvalidOption(
                            "`--s3-endpoint` is required when `--s3-mode external` is set".to_string(),
                        ))
                    }
                };
                let access = match args.s3_access_key.as_ref() {
                    Some(v) if !v.trim().is_empty() => Some(v.clone()),
                    _ if interactive => Some(Self::prompt_required(&mut stdin, &mut stdout, "External S3 Access Key")?),
                    _ => {
                        return Err(CliError::InvalidOption(
                            "`--s3-access-key` is required when `--s3-mode external` is set".to_string(),
                        ))
                    }
                };
                let secret = match args.s3_secret_key.as_ref() {
                    Some(v) if !v.trim().is_empty() => Some(v.clone()),
                    _ if interactive => Some(Self::prompt_required(&mut stdin, &mut stdout, "External S3 Secret Key")?),
                    _ => {
                        return Err(CliError::InvalidOption(
                            "`--s3-secret-key` is required when `--s3-mode external` is set".to_string(),
                        ))
                    }
                };
                (endpoint, access, secret)
            }
        };

        Ok(Self {
            cluster_id,
            image,
            proxy_port,
            dashboard_bind: args.dashboard_bind.clone(),
            dashboard_port,
            superuser,
            s3_mode,
            s3_api_port,
            s3_console_port,
            s3_endpoint,
            s3_bucket,
            s3_region,
            s3_access_key,
            s3_secret_key,
            tls: args.tls,
            force: args.force,
            start: args.start,
        })
    }

    fn prompt_str(
        stdin: &mut impl BufRead,
        stdout: &mut impl Write,
        label: &str,
        default: &str,
    ) -> Result<String, CliError> {
        write!(stdout, "{label} [{default}]: ").map_err(|source| CliError::Io {
            path: "stdout".into(),
            source,
        })?;
        stdout.flush().map_err(|source| CliError::Io {
            path: "stdout".into(),
            source,
        })?;
        let mut line = String::new();
        stdin.read_line(&mut line).map_err(|source| CliError::Io {
            path: "stdin".into(),
            source,
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            Ok(default.to_string())
        } else {
            Ok(trimmed.to_string())
        }
    }

    fn prompt_required(
        stdin: &mut impl BufRead,
        stdout: &mut impl Write,
        label: &str,
    ) -> Result<String, CliError> {
        write!(stdout, "{label}: ").map_err(|source| CliError::Io {
            path: "stdout".into(),
            source,
        })?;
        stdout.flush().map_err(|source| CliError::Io {
            path: "stdout".into(),
            source,
        })?;
        let mut line = String::new();
        stdin.read_line(&mut line).map_err(|source| CliError::Io {
            path: "stdin".into(),
            source,
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            Err(CliError::InvalidOption(format!("{label} cannot be empty")))
        } else {
            Ok(trimmed.to_string())
        }
    }

    fn prompt_u16(
        stdin: &mut impl BufRead,
        stdout: &mut impl Write,
        label: &str,
        default: u16,
    ) -> Result<u16, CliError> {
        let raw = Self::prompt_str(stdin, stdout, label, &default.to_string())?;
        raw.parse::<u16>()
            .map_err(|_| CliError::InvalidOption(format!("Invalid port '{raw}' for {label}")))
    }
}
