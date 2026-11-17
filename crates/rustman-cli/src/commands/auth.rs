//! `rustman auth login <request>`: run a request's OAuth2 flow and cache the
//! token in the secret store, so later runs (CI included) can reuse and
//! refresh it without a browser.

use clap::{Args, Subcommand};
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{ConfigError, SecretBackend, SecretStore};
use rustman_http::{HttpEngine, Login, OAuth2Context, SendOptions, oauth2_cache_key};

use super::exec::{runtime, show_login_url};
use super::{EnvSelection, current_dir, env_label, print_warnings};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Log in with a request's OAuth2 auth and store the token
    Login(LoginArgs),
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// The request: a request file, its id (path without .rman.yaml) or its name
    request: String,
    #[command(flatten)]
    selection: EnvSelection,
}

pub fn run(args: &AuthArgs) -> Result<(), CliError> {
    match &args.command {
        AuthCommand::Login(a) => login(a),
    }
}

fn login(args: &LoginArgs) -> Result<(), CliError> {
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let target = ws.find_request(&args.request, &current_dir()?)?;
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    let store = SecretStore::open(&ws.root, backend);
    let resolver = Resolver::for_request(&ws.settings, &target, env, &store);
    let request = resolve_request(&target, &resolver)?;
    let options = SendOptions::for_request(&ws.settings, &ws.root, &request)?;
    let context = OAuth2Context {
        store: &store,
        env: env.map(|e| e.name.as_str()),
        cache_key: oauth2_cache_key(&target),
        login: Login::Interactive(&show_login_url),
    };
    let result = runtime()?.block_on(HttpEngine::new().oauth2_login(&request, &options, &context));
    print_warnings(&store.take_warnings());
    result?;
    println!(
        "logged in: token for '{}' stored for {}",
        target.entry.id(),
        env_label(env)
    );
    Ok(())
}
