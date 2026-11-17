mod commands;
mod exit;

use std::io::Write;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rustman", version = rustman_core::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new workspace (rustman.toml, a local environment and a sample request)
    Init(commands::init::InitArgs),
    /// Inspect environments
    Env(commands::env::EnvArgs),
    /// Manage secret values (stored in the OS keychain, never in workspace files)
    Secret(commands::secret::SecretArgs),
    /// Manage the cookie jars kept per environment
    Cookies(commands::cookies::CookiesArgs),
    /// Log in to OAuth2-protected APIs
    Auth(commands::auth::AuthArgs),
    /// Create request files from other formats (curl)
    Import(commands::import::ImportArgs),
    /// Print requests in other formats (curl)
    Export(commands::export::ExportArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        // The TUI arrives in a later change; until then, show help. A closed
        // stdout (e.g. `rustman | head -c0`) is not an error worth a panic.
        let mut out = std::io::stdout();
        let _ = Cli::command()
            .write_help(&mut out)
            .and_then(|()| writeln!(out));
        return ExitCode::SUCCESS;
    };
    let result: Result<(), exit::CliError> = match command {
        Command::Init(args) => commands::init::run(&args).map_err(Into::into),
        Command::Env(args) => commands::env::run(&args).map_err(Into::into),
        Command::Secret(args) => commands::secret::run(&args).map_err(Into::into),
        Command::Cookies(args) => commands::cookies::run(&args).map_err(Into::into),
        Command::Auth(args) => commands::auth::run(&args),
        Command::Import(args) => commands::import::run(&args),
        Command::Export(args) => commands::export::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{}", err.render());
            ExitCode::from(err.code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
