//! `rustman export curl <request>`: print a request as a curl command, with
//! secrets redacted unless `--show-secrets`.

use clap::{Args, Subcommand};
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{ConfigError, SecretBackend, SecretStore};
use rustman_http::export_curl;

use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    command: ExportCommand,
}

#[derive(Debug, Subcommand)]
enum ExportCommand {
    /// Print a request as a single curl command
    Curl(CurlArgs),
}

#[derive(Debug, Args)]
struct CurlArgs {
    /// The request: a request file, its id (path without .rman.yaml) or its name
    request: String,
    /// Print secret values instead of «redacted:<name>» placeholders
    #[arg(long)]
    show_secrets: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

pub fn run(args: &ExportArgs) -> Result<(), CliError> {
    match &args.command {
        ExportCommand::Curl(a) => curl(a),
    }
}

fn curl(args: &CurlArgs) -> Result<(), CliError> {
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let target = ws.find_request(&args.request, &current_dir()?)?;
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    let store = SecretStore::open(&ws.root, backend);
    let resolver = Resolver::for_request(&ws.settings, &target, env, &store);
    let resolved = resolve_request(&target, &resolver);
    print_warnings(&store.take_warnings());
    let exported = export_curl(&resolved?, args.show_secrets);
    print_warnings(&exported.warnings);
    println!("{}", exported.command);
    Ok(())
}
