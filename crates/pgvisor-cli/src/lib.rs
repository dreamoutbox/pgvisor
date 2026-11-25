pub mod cli;
pub mod error;

pub use cli::{
    CleanArgs, CliArgs, CliCommand, LogsArgs, RestartArgs, S3Mode, SetupArgs, UpArgs,
};
pub use error::CliError;

/// Dispatches parsed CLI arguments to the selected subcommand handler.
pub fn run(args: CliArgs) -> Result<(), CliError> {
    match args.command {
        CliCommand::Setup(_) => Err(CliError::NotImplemented("setup")),
        CliCommand::Up(_) => Err(CliError::NotImplemented("up")),
        CliCommand::Down => Err(CliError::NotImplemented("down")),
        CliCommand::Restart(_) => Err(CliError::NotImplemented("restart")),
        CliCommand::Status => Err(CliError::NotImplemented("status")),
        CliCommand::Logs(_) => Err(CliError::NotImplemented("logs")),
        CliCommand::Clean(_) => Err(CliError::NotImplemented("clean")),
    }
}
