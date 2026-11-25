pub mod cli;
pub mod docker;
pub mod error;
pub mod setup;

pub use cli::{
    CleanArgs, CliArgs, CliCommand, LogsArgs, RestartArgs, S3Mode, SetupArgs, UpArgs,
};
pub use error::CliError;

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
        CliCommand::Up(_) => Err(CliError::NotImplemented("up")),
        CliCommand::Down => Err(CliError::NotImplemented("down")),
        CliCommand::Restart(_) => Err(CliError::NotImplemented("restart")),
        CliCommand::Status => Err(CliError::NotImplemented("status")),
        CliCommand::Logs(_) => Err(CliError::NotImplemented("logs")),
        CliCommand::Clean(_) => Err(CliError::NotImplemented("clean")),
    }
}
