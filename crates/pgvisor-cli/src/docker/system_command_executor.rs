use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::docker::command_executor::CommandExecutor;
use crate::docker::command_output::CommandOutput;
use crate::error::CliError;

/// Default `CommandExecutor` backed by `std::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCommandExecutor;

impl SystemCommandExecutor {
    fn format_cmd(program: &str, args: &[&str]) -> String {
        if args.is_empty() {
            program.to_string()
        } else {
            format!("{program} {}", args.join(" "))
        }
    }
}

impl CommandExecutor for SystemCommandExecutor {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CliError> {
        let output = Command::new(program).args(args).output().map_err(|source| {
            CliError::Io {
                path: PathBuf::from(program),
                source,
            }
        })?;

        Ok(CommandOutput {
            status_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn run_interactive(&self, program: &str, args: &[&str]) -> Result<i32, CliError> {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|source| CliError::Io {
                path: PathBuf::from(program),
                source,
            })?;

        let code = status.code().unwrap_or(-1);
        if code != 0 {
            return Err(CliError::CommandFailed {
                command: Self::format_cmd(program, args),
                status_code: code,
                stderr: "interactive command exited non-zero".to_string(),
            });
        }
        Ok(code)
    }

    fn is_in_path(&self, program: &str) -> bool {
        let Some(paths) = env::var_os("PATH") else {
            return false;
        };
        env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    }
}
