//! `rustman mcp-client`: explore and call MCP servers from scripts
//! (mcp-client "MCP client CLI"). `<server>` is a server the workspace
//! defines, `stdio:<command line>` or an `http(s)://` URL. The client lives
//! in `rustman_mcp::client`; this command finds the workspace, connects
//! headless (sampling rejected unless `--sampling-response`, elicitation
//! declined, each with a warning) and prints the result as text or JSON.
//!
//! Exit codes: 0 on success, 1 for an `isError` tool result or a protocol
//! error, 2 for configuration and trust problems, 3 when the server cannot
//! be reached or does not answer in time.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::environment::ENV_VAR;
use rustman_core::vars::{EnvironmentSource, Layer, Resolver, VariablesSource};
use rustman_core::{ConfigError, SecretStore, Workspace};
use rustman_mcp::client::oauth::{OAuthOptions, OAuthPrompt, logout};
use rustman_mcp::client::output::printable;
use rustman_mcp::client::{
    CallOptions, ClientError, ConnectOptions, HeadlessBroker, McpOp, McpSession, OpResult,
    RequestOptions, ResolvedServer, ServerTarget, load_sampling_response,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::exec::{cancel_on_ctrl_c, parse_duration, runtime, secret_store, show_login_url};
use super::{current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct McpClientArgs {
    #[command(subcommand)]
    command: McpClientCommand,
}

#[derive(Debug, Subcommand)]
enum McpClientCommand {
    /// List the tools of an MCP server
    Tools(ServerArgs),
    /// Call a tool of an MCP server
    Call(CallArgs),
    /// List the resources of an MCP server, or read one
    Resources(ResourcesArgs),
    /// List the prompts of an MCP server, or get one
    Prompts(PromptsArgs),
    /// Delete the OAuth tokens stored for an MCP server
    Logout(LogoutArgs),
}

#[derive(Debug, Args)]
struct ServerArgs {
    /// A server of the workspace, stdio:<command line> or an http(s):// URL
    #[arg(value_name = "SERVER")]
    server: String,
    /// Print the result as JSON
    #[arg(long)]
    json: bool,
    /// Spawn a stdio server that is not trusted yet, and record the approval
    #[arg(long)]
    trust: bool,
    /// Give up on a request after this long (default: the server's
    /// timeout_ms, else 60s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Environment (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CallArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// The tool to call
    #[arg(value_name = "TOOL")]
    tool: String,
    /// The arguments as one JSON object
    #[arg(long, value_name = "JSON", conflicts_with = "arg")]
    args: Option<String>,
    /// One argument: k=v for a string, k:=JSON for any JSON value (repeatable)
    #[arg(long, value_name = "K=V")]
    arg: Vec<String>,
    /// Send arguments that break the tool's input schema anyway
    #[arg(long)]
    no_validate: bool,
    /// Answer sampling requests with the CreateMessageResult in FILE
    /// (default: reject them)
    #[arg(long, value_name = "FILE")]
    sampling_response: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ResourcesArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Read the resource at URI instead of listing
    #[arg(long, value_name = "URI")]
    read: Option<String>,
}

#[derive(Debug, Args)]
struct PromptsArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Get the prompt NAME instead of listing
    #[arg(long, value_name = "NAME")]
    get: Option<String>,
    /// One argument of --get: k=v (repeatable)
    #[arg(long, value_name = "K=V", requires = "get")]
    arg: Vec<String>,
}

#[derive(Debug, Args)]
struct LogoutArgs {
    /// A server of the workspace or an http(s):// URL
    #[arg(value_name = "SERVER")]
    server: String,
    /// Environment (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

pub fn run(args: &McpClientArgs) -> Result<u8, CliError> {
    match &args.command {
        McpClientCommand::Tools(args) => perform(args, &McpOp::ToolsList, None),
        McpClientCommand::Call(args) => call(args),
        McpClientCommand::Resources(args) => {
            let op = match &args.read {
                Some(uri) => McpOp::ResourcesRead { uri: uri.clone() },
                None => McpOp::ResourcesList,
            };
            perform(&args.server, &op, None)
        }
        McpClientCommand::Prompts(args) => {
            let op = match &args.get {
                Some(name) => McpOp::PromptsGet {
                    name: name.clone(),
                    arguments: string_arguments(&args.arg)?,
                },
                None => McpOp::PromptsList,
            };
            perform(&args.server, &op, None)
        }
        McpClientCommand::Logout(args) => sign_out(args),
    }
}

fn call(args: &CallArgs) -> Result<u8, CliError> {
    let arguments = match &args.args {
        Some(json) => serde_json::from_str::<Value>(json)
            .map_err(|e| usage(format!("--args: invalid JSON: {e}")))?,
        None => Value::Object(json_arguments(&args.arg)?),
    };
    if !arguments.is_object() {
        return Err(usage("--args must be a JSON object".into()));
    }
    let op = McpOp::ToolsCall {
        name: args.tool.clone(),
        arguments,
    };
    let call = Call {
        validate: !args.no_validate,
        sampling_response: args.sampling_response.as_deref(),
    };
    perform(&args.server, &op, Some(call))
}

/// What only `call` sets.
#[derive(Clone, Copy)]
struct Call<'a> {
    validate: bool,
    sampling_response: Option<&'a Path>,
}

/// Where the command runs: the workspace, or the current folder for an
/// ad-hoc server outside one.
struct Place {
    root: PathBuf,
    workspace: Option<Workspace>,
}

impl Place {
    fn find(explicit: Option<&Path>, target: &ServerTarget) -> Result<Self, ConfigError> {
        let cwd = current_dir()?;
        match super::plugins::discover(&cwd, explicit) {
            Ok(ws) => {
                print_warnings(&ws.warnings);
                Ok(Self {
                    root: ws.root.clone(),
                    workspace: Some(ws),
                })
            }
            Err(ConfigError::NoWorkspace) if target.ad_hoc().is_some() => Ok(Self {
                root: cwd,
                workspace: None,
            }),
            Err(e) => Err(e),
        }
    }
}

fn perform(args: &ServerArgs, op: &McpOp, call: Option<Call<'_>>) -> Result<u8, CliError> {
    let target = ServerTarget::parse(&args.server).map_err(mcp)?;
    let place = Place::find(args.workspace.as_deref(), &target)?;
    let store = Arc::new(secret_store(&place.root)?);
    let server = resolve(args, &target, &place, &store)?;
    let connect = ConnectOptions {
        timeout: args.timeout,
        ..connect_options(call, store.clone())?
    };
    let cancel = CancellationToken::new();
    let options = CallOptions {
        request: RequestOptions {
            timeout: args.timeout,
            cancel: Some(cancel.clone()),
            ..RequestOptions::default()
        },
        validate: call.is_none_or(|c| c.validate),
    };
    let (session, approved) = server;
    // Ctrl+C cancels the request and stops the server; a server still
    // starting is stopped when the runtime drops its process watcher.
    let done = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        let session = tokio::select! {
            session = McpSession::connect(&session, &approved, &connect) => session?,
            () = cancel.cancelled() => return Err(ClientError::Cancelled),
        };
        let done = session.perform(op, &options).await;
        session.shutdown().await;
        done
    });
    print_warnings(&store.take_warnings());
    let done = done.map_err(mcp)?;
    print_result(&done, args.json)?;
    Ok(if done.is_error { ASSERTION } else { SUCCESS })
}

fn resolve(
    args: &ServerArgs,
    target: &ServerTarget,
    place: &Place,
    store: &SecretStore,
) -> Result<(ResolvedServer, rustman_mcp::client::Approved), CliError> {
    let resolver = resolver(place, args.env.as_deref(), store)?;
    target
        .resolve(
            &place.root,
            place.workspace.is_some(),
            &resolver,
            args.trust,
        )
        .map_err(mcp)
}

/// The variables of server definitions: the environment's, then
/// `rustman.toml`'s (none outside a workspace).
fn resolver<'a>(
    place: &'a Place,
    env: Option<&str>,
    store: &'a SecretStore,
) -> Result<Resolver<'a>, CliError> {
    let mut resolver = Resolver::new();
    if let Some(ws) = &place.workspace {
        let env_var = std::env::var(ENV_VAR).ok();
        if let Some(env) = ws.active_environment(env, env_var.as_deref())? {
            resolver.push(EnvironmentSource::new(env, store));
        }
        resolver.push(VariablesSource::new(
            Layer::Settings,
            &ws.settings.variables,
        ));
    }
    Ok(resolver)
}

/// Headless: warnings on stderr, sampling rejected unless a canned answer is
/// given; OAuth logins only with a terminal to show the URL on.
fn connect_options(
    call: Option<Call<'_>>,
    store: Arc<SecretStore>,
) -> Result<ConnectOptions, CliError> {
    let mut broker = HeadlessBroker::new(|warning| eprintln!("warning: {warning}"));
    if let Some(path) = call.and_then(|c| c.sampling_response) {
        broker = broker.with_sampling_response(load_sampling_response(path).map_err(mcp)?);
    }
    let prompt: Option<Arc<dyn OAuthPrompt>> = std::io::stdin()
        .is_terminal()
        .then(|| Arc::new(TerminalPrompt) as Arc<dyn OAuthPrompt>);
    Ok(ConnectOptions {
        broker: Arc::new(broker),
        oauth: Some(OAuthOptions { store, prompt }),
        ..ConnectOptions::default()
    })
}

/// How `rustman run` connects to MCP servers: headless (the runner
/// replaces the broker), with OAuth tokens from the workspace's store.
pub fn run_connect_options(store: Arc<SecretStore>) -> ConnectOptions {
    let prompt: Option<Arc<dyn OAuthPrompt>> = std::io::stdin()
        .is_terminal()
        .then(|| Arc::new(TerminalPrompt) as Arc<dyn OAuthPrompt>);
    ConnectOptions {
        oauth: Some(OAuthOptions { store, prompt }),
        ..ConnectOptions::default()
    }
}

/// Shows the authorization URL and opens the browser.
struct TerminalPrompt;

impl OAuthPrompt for TerminalPrompt {
    fn open(&self, url: &str) {
        show_login_url(url);
    }
}

fn print_result(done: &OpResult, json: bool) -> Result<(), CliError> {
    let text = if json {
        let mut text = serde_json::to_string_pretty(&done.result).unwrap_or_default();
        text.push('\n');
        text
    } else {
        printable(&done.text).into_owned()
    };
    let mut out = std::io::stdout().lock();
    // A closed stdout (`| head`) is not an error worth reporting.
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    Ok(())
}

fn sign_out(args: &LogoutArgs) -> Result<u8, CliError> {
    let target = ServerTarget::parse(&args.server).map_err(mcp)?;
    let place = Place::find(args.workspace.as_deref(), &target)?;
    let store = secret_store(&place.root)?;
    // Logging out runs nothing: no trust is needed, none is recorded.
    let resolver = resolver(&place, args.env.as_deref(), &store)?;
    let server = target
        .definition(&place.root, place.workspace.is_some())
        .and_then(|d| d.resolve(&resolver, &place.root))
        .map_err(mcp)?;
    let removed = logout(&store, &server).map_err(mcp)?;
    print_warnings(&store.take_warnings());
    if removed {
        println!("Logged out of {}", server.name);
    } else {
        println!("No stored tokens for {}", server.name);
    }
    Ok(SUCCESS)
}

/// `k=v` (a string) and `k:=JSON` (any value) pairs as a JSON object.
fn json_arguments(pairs: &[String]) -> Result<Map<String, Value>, CliError> {
    let mut arguments = Map::new();
    for pair in pairs {
        let (key, value) = match pair.split_once(":=") {
            Some((key, json)) if !key.contains('=') => {
                let value = serde_json::from_str(json)
                    .map_err(|e| usage(format!("--arg {pair}: invalid JSON: {e}")))?;
                (key, value)
            }
            _ => {
                let (key, text) = split_pair(pair)?;
                (key, Value::String(text.to_owned()))
            }
        };
        arguments.insert(key.to_owned(), value);
    }
    Ok(arguments)
}

/// `k=v` pairs (prompt arguments are strings).
fn string_arguments(
    pairs: &[String],
) -> Result<std::collections::BTreeMap<String, String>, CliError> {
    pairs
        .iter()
        .map(|pair| split_pair(pair).map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect()
}

fn split_pair(pair: &str) -> Result<(&str, &str), CliError> {
    match pair.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key, value)),
        _ => Err(usage(format!("--arg {pair}: expected K=V"))),
    }
}

fn usage(message: String) -> CliError {
    ConfigError::Usage(message).into()
}

fn mcp(err: ClientError) -> CliError {
    CliError::Mcp(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arguments_are_strings_or_json() {
        let pairs = [
            "path=a b".to_owned(),
            "n:=3".to_owned(),
            "eq=x=y".to_owned(),
        ];
        let arguments = json_arguments(&pairs).unwrap();
        assert_eq!(
            Value::Object(arguments),
            json!({"path": "a b", "n": 3, "eq": "x=y"})
        );
        assert!(json_arguments(&["novalue".to_owned()]).is_err());
        assert!(json_arguments(&["n:={".to_owned()]).is_err());
        let strings = string_arguments(&["code=fn main() {}".to_owned()]).unwrap();
        assert_eq!(strings["code"], "fn main() {}");
    }
}
