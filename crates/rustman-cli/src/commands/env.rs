//! `rustman env list`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use rustman_core::environment::ENV_VAR;
use rustman_core::{ConfigError, Workspace};

#[derive(Debug, Args)]
pub struct EnvArgs {
    #[command(subcommand)]
    command: EnvCommand,
}

#[derive(Debug, Subcommand)]
enum EnvCommand {
    /// List the workspace's environments, marking the active one with '*'
    List(ListArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Environment to treat as active (overrides RUSTMAN_ENV and default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

pub fn run(args: &EnvArgs) -> Result<(), ConfigError> {
    match &args.command {
        EnvCommand::List(list) => run_list(list),
    }
}

fn run_list(args: &ListArgs) -> Result<(), ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::Io {
        path: PathBuf::from("."),
        source: e,
    })?;
    let ws = Workspace::discover(&cwd, args.workspace.as_deref())?;
    super::print_warnings(&ws.warnings);
    let env_var = std::env::var(ENV_VAR).ok();
    let active = ws.active_environment(args.env.as_deref(), env_var.as_deref())?;
    let active = active.map(|e| e.name.as_str());
    for env in &ws.environments {
        let marker = if Some(env.name.as_str()) == active {
            '*'
        } else {
            ' '
        };
        println!("{marker} {}", env.name);
    }
    Ok(())
}
