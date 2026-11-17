mod commands;
mod exit;
mod terminal;

use std::io::Write;
use std::process::ExitCode;

use clap::{ArgAction, CommandFactory, Parser, Subcommand};

use terminal::ColorWhen;

#[derive(Debug, Parser)]
#[command(name = "rustman", version = rustman_core::VERSION, about)]
struct Cli {
    /// Color: auto (a terminal without NO_COLOR or TERM=dumb), always or never
    #[arg(
        long,
        value_name = "WHEN",
        value_enum,
        global = true,
        default_value_t,
        hide_possible_values = true,
        display_order = 100
    )]
    color: ColorWhen,
    /// Log more on stderr: -v requests, -vv headers, -vvv everything
    /// (RUST_LOG, when set, replaces these levels)
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count, global = true,
          display_order = 101)]
    verbose: u8,
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
    /// Send one request (a file, id or name, or an ad-hoc METHOD URL) and print the response
    Send(commands::send::SendArgs),
    /// Run the requests under files or folders and report the results
    Run(commands::run::RunArgs),
    /// List, inspect, replay and clear past exchanges (redacted)
    History(commands::history::HistoryArgs),
    /// Print a shell completion script: rustman completions zsh > _rustman
    Completions(commands::completions::CompletionsArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    terminal::init_color(cli.color);
    terminal::init_logs(cli.verbose, cli.color);
    let Some(command) = cli.command else {
        // The TUI arrives in a later change; until then, show help. A closed
        // stdout (e.g. `rustman | head -c0`) is not an error worth a panic.
        let mut out = std::io::stdout();
        let _ = Cli::command()
            .write_help(&mut out)
            .and_then(|()| writeln!(out));
        return ExitCode::SUCCESS;
    };
    match dispatch(command) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{}", err.render());
            ExitCode::from(err.code())
        }
    }
}

/// Run `command`; `Ok` carries the exit code of a command that completed.
fn dispatch(command: Command) -> Result<u8, exit::CliError> {
    let done = |()| exit::SUCCESS;
    match command {
        Command::Init(args) => Ok(commands::init::run(&args).map(done)?),
        Command::Env(args) => Ok(commands::env::run(&args).map(done)?),
        Command::Secret(args) => Ok(commands::secret::run(&args).map(done)?),
        Command::Cookies(args) => Ok(commands::cookies::run(&args).map(done)?),
        Command::Auth(args) => commands::auth::run(&args).map(done),
        Command::Import(args) => commands::import::run(&args).map(done),
        Command::Export(args) => commands::export::run(&args).map(done),
        Command::Send(args) => commands::send::run(&args),
        Command::Run(args) => commands::run::run(&args),
        Command::History(args) => commands::history::run(&args),
        Command::Completions(args) => {
            commands::completions::run(&args, &mut Cli::command());
            Ok(exit::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests;
