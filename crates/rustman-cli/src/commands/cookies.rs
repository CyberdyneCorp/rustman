//! `rustman cookies clear`: empty the cookie jar of one environment.

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_http::CookieJar;

use super::{EnvSelection, env_label};

#[derive(Debug, Args)]
pub struct CookiesArgs {
    #[command(subcommand)]
    command: CookiesCommand,
}

#[derive(Debug, Subcommand)]
enum CookiesCommand {
    /// Delete every cookie stored for an environment (.rustman/cookies/<env>.json)
    Clear(EnvSelection),
}

pub fn run(args: &CookiesArgs) -> Result<(), ConfigError> {
    match &args.command {
        CookiesCommand::Clear(selection) => clear(selection),
    }
}

fn clear(selection: &EnvSelection) -> Result<(), ConfigError> {
    let ws = selection.workspace()?;
    let env = selection.environment(&ws)?;
    let name = env.map(|e| e.name.as_str());
    let removed = CookieJar::clear(&ws.root, name).map_err(|e| ConfigError::Io {
        path: CookieJar::path_for(&ws.root, name),
        source: e,
    })?;
    let label = env_label(env);
    if removed {
        println!("cleared cookies for {label}");
    } else {
        println!("no cookies stored for {label}");
    }
    Ok(())
}
