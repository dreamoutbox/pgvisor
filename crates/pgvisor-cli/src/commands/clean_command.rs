use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::cli::CleanArgs;
use crate::docker::{CommandExecutor, ComposeRunner};
use crate::error::CliError;

/// Executes `pgvisor clean` to stop containers and delete persistent database volumes.
pub struct CleanCommand<'a> {
    executor: &'a dyn CommandExecutor,
}

impl<'a> CleanCommand<'a> {
    /// Creates a `CleanCommand` using the given `CommandExecutor`.
    pub fn new(executor: &'a dyn CommandExecutor) -> Self {
        Self { executor }
    }

    /// Prompts via `stdin` unless `--yes` is set, then runs `docker compose down -v --remove-orphans`.
    pub fn execute_with_reader(
        &self,
        dir: &Path,
        args: &CleanArgs,
        silent: bool,
        reader: &mut impl BufRead,
    ) -> Result<bool, CliError> {
        let compose = ComposeRunner::new(self.executor, dir);
        if !compose.compose_file().is_file() {
            return Err(CliError::MissingComposeFile(
                compose.compose_file().to_path_buf(),
            ));
        }

        if !args.yes {
            print!("WARNING: This will permanently delete all PostgreSQL database volumes. Continue? [y/N] ");
            io::stdout().flush().map_err(|source| CliError::Io {
                path: "stdout".into(),
                source,
            })?;
            let mut answer = String::new();
            reader.read_line(&mut answer).map_err(|source| CliError::Io {
                path: "stdin".into(),
                source,
            })?;
            let normalized = answer.trim().to_ascii_lowercase();
            if normalized != "y" && normalized != "yes" {
                if !silent {
                    println!("Operation cancelled.");
                }
                return Ok(false);
            }
        }

        if !silent {
            println!("Stopping containers and deleting persistent database volumes...");
        }
        compose.clean()?;
        if !silent {
            println!("Cluster volumes removed.");
        }
        Ok(true)
    }

    /// Executes `clean` reading confirmation from standard input.
    pub fn execute(&self, dir: &Path, args: &CleanArgs, silent: bool) -> Result<bool, CliError> {
        let mut stdin = io::stdin().lock();
        self.execute_with_reader(dir, args, silent, &mut stdin)
    }
}
