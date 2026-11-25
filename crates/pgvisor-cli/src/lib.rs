pub mod cli;
pub mod commands;
pub mod config;
pub mod docker;
pub mod error;
pub mod setup;

pub use cli::{
    CleanArgs, CliArgs, CliCommand, LogsArgs, RestartArgs, S3Mode, SetupArgs, UpArgs,
};
pub use error::CliError;

use commands::{
    CleanCommand, DownCommand, LogsCommand, RestartCommand, StatusCommand, UpCommand,
};
use docker::SystemCommandExecutor;
use setup::SetupCommand;

/// Dispatches parsed CLI arguments to the selected subcommand handler.
pub fn run(args: CliArgs) -> Result<(), CliError> {
    let executor = SystemCommandExecutor;
    match args.command {
        CliCommand::Setup(setup_args) => {
            SetupCommand::new(&executor).execute(&args.dir, &setup_args, args.silent)?;
            Ok(())
        }
        CliCommand::Up(up_args) => UpCommand::new(&executor).execute(&args.dir, &up_args, args.silent),
        CliCommand::Down => DownCommand::new(&executor).execute(&args.dir, args.silent),
        CliCommand::Restart(restart_args) => {
            RestartCommand::new(&executor).execute(&args.dir, &restart_args, args.silent)
        }
        CliCommand::Status => StatusCommand::new(&executor).execute(&args.dir),
        CliCommand::Logs(logs_args) => LogsCommand::new(&executor).execute(&args.dir, &logs_args),
        CliCommand::Clean(clean_args) => {
            CleanCommand::new(&executor).execute(&args.dir, &clean_args, args.silent)?;
            Ok(())
        }
    }
}
