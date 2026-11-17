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
    let result = match command {
        Command::Init(args) => commands::init::run(&args),
        Command::Env(args) => commands::env::run(&args),
        Command::Secret(args) => commands::secret::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{}", exit::render(&err));
            ExitCode::from(exit::code(&err))
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
