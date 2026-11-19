//! `rustman mock serve`: serve an OpenAPI document as a mock until Ctrl+C
//! (add-openapi-mock-server). Settings come from flags, then `[mock]` in
//! `rustman.toml` when a workspace is found, then the defaults. Overrides
//! are read from the mocks directory (`[mock] dir`, else `mocks/` in the
//! workspace or the current directory); expected security values from the
//! active environment's secrets.
//!
//! Each request is logged as one human line on stderr, or one JSON object
//! on stdout with `--jsonl`, redacted; `--history` also records it in
//! `request-history` under `mock`. The document and the mocks directory
//! are watched and reloaded on change. `--proxy-unmatched` forwards what
//! nothing matches, and `--record` saves those exchanges as request files.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::environment::ENV_VAR;
use rustman_core::history::HistoryStore;
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::settings::Settings;
use rustman_core::{ConfigError, SecretBackend, Workspace};
use rustman_import::ImportError;
use rustman_mock::record::RecordSetupError;
use rustman_mock::server::{bind_addr, remote_warning};
use rustman_mock::watch::watch;
use rustman_mock::{
    MockConfig, MockEvent, MockHandle, MockServer, MockSettings, Overrides, Recorder, Reload,
    Upstream, security,
};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;

use super::current_dir;
use super::exec::{cancel_on_ctrl_c, history_store, runtime, secret_store};
use super::import::destination;
use crate::exit::{CliError, INTERRUPTED};

/// How long requests in flight may take to finish after Ctrl+C.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct MockArgs {
    #[command(subcommand)]
    command: MockCommand,
}

#[derive(Debug, Subcommand)]
enum MockCommand {
    /// Serve an OpenAPI document as a mock server until Ctrl+C
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// The OpenAPI 3.0/3.1 document: a file or an http(s) URL
    #[arg(value_name = "OPENAPI")]
    openapi: String,
    /// Address to bind: an IP address or localhost (default: [mock] host in
    /// rustman.toml, else 127.0.0.1)
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// Port to bind; 0 picks a free port (default: [mock] port in
    /// rustman.toml, else 4010)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Allow binding an address reachable from the network
    #[arg(long)]
    allow_remote: bool,
    /// Fetch $refs to other http(s) documents
    #[arg(long)]
    allow_remote_refs: bool,
    /// Seed of synthesized bodies (default: [mock] seed, else 0)
    #[arg(long, value_name = "N")]
    seed: Option<u64>,
    /// Ignore the Prefer request header
    #[arg(long)]
    no_prefer: bool,
    /// Do not validate requests against the document
    #[arg(long)]
    no_validate: bool,
    /// Answer 401 to requests without the credentials their operation
    /// declares
    #[arg(long)]
    enforce_security: bool,
    /// Keep an in-memory store: POST creates, GET reads, PUT/PATCH update,
    /// DELETE removes
    #[arg(long)]
    stateful: bool,
    /// Environment holding the [mock.security] expected values and
    /// receiving the secrets of --record (default: RUSTMAN_ENV, else the
    /// workspace default)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace whose rustman.toml holds [mock] (default: search from the
    /// current directory upwards; none means the defaults)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Add CORS headers and answer preflights (default: on for loopback)
    #[arg(long, overrides_with = "no_cors")]
    cors: bool,
    /// Never add CORS headers; OPTIONS goes to the document
    #[arg(long)]
    no_cors: bool,
    /// Log each request as one JSON object per line on stdout (the banner
    /// goes to stderr)
    #[arg(long)]
    jsonl: bool,
    /// Also record each exchange in history, tagged mock
    #[arg(long)]
    history: bool,
    /// Forward requests no operation or override matches to this upstream
    /// (only its origin is contacted)
    #[arg(long, value_name = "URL", value_parser = parse_upstream)]
    proxy_unmatched: Option<Upstream>,
    /// Save each proxied exchange as a request file in DIR, inside the
    /// workspace, with secrets moved to the active environment
    #[arg(long, value_name = "DIR", requires = "proxy_unmatched")]
    record: Option<PathBuf>,
}

fn parse_upstream(text: &str) -> Result<Upstream, String> {
    Upstream::parse(text).map_err(|e| e.to_string())
}

pub fn run(args: &MockArgs) -> Result<u8, CliError> {
    match &args.command {
        MockCommand::Serve(serve) => run_serve(serve),
    }
}

fn run_serve(args: &ServeArgs) -> Result<u8, CliError> {
    let setup = setup(args)?;
    runtime()?.block_on(serve(args, setup))
}

/// What the mock starts with besides the document.
struct Setup {
    config: MockConfig,
    overrides: Overrides,
    mocks_dir: PathBuf,
    root: Option<PathBuf>,
    recorder: Option<Recorder>,
}

fn setup(args: &ServeArgs) -> Result<Setup, CliError> {
    let invalid = |e| ConfigError::Invalid(vec![e]);
    let root = workspace_root(args.workspace.as_deref())?;
    let settings = match &root {
        Some(root) => MockSettings::load(root).map_err(invalid)?,
        None => MockSettings::default(),
    };
    settings.latency().map_err(invalid)?;
    let mut config = config(args, &settings);
    // Refuse a network-reachable address before reading anything else.
    bind_addr(&config)?;
    let base = match &root {
        Some(root) => root.clone(),
        None => current_dir()?,
    };
    let mocks_dir = settings.mocks_dir(&base);
    let overrides = Overrides::load_dir(&mocks_dir).map_err(invalid)?;
    if let Some(root) = &root {
        let env_var = std::env::var(ENV_VAR).ok();
        config.security = security::load(
            root,
            &settings.security,
            args.env.as_deref(),
            env_var.as_deref(),
            || secret_store(root),
        )?;
    }
    let recorder = match &args.record {
        Some(dir) => Some(recorder(args, root.as_deref(), dir)?),
        None => None,
    };
    Ok(Setup {
        config,
        overrides,
        mocks_dir,
        root,
        recorder,
    })
}

/// `--record DIR`: inside the workspace, secrets to the active environment.
fn recorder(args: &ServeArgs, root: Option<&Path>, dir: &Path) -> Result<Recorder, CliError> {
    let root = root.ok_or_else(|| {
        ConfigError::Usage("--record needs a workspace (rustman init, or --workspace)".into())
    })?;
    let dest = destination(root, dir).map_err(|e| match e {
        CliError::Import(ImportError::OutsideWorkspace) => {
            let outside = RecordSetupError::Outside(dir.display().to_string());
            CliError::from(ConfigError::Usage(outside.to_string()))
        }
        other => other,
    })?;
    let ws = Workspace::load(root)?;
    let env_var = std::env::var(ENV_VAR).ok();
    let environment = ws
        .active_environment(args.env.as_deref(), env_var.as_deref())?
        .map(|e| e.name.clone());
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    Recorder::new(root, &dest, environment, backend)
        .map_err(|e| ConfigError::Usage(e.to_string()).into())
}

async fn serve(args: &ServeArgs, setup: Setup) -> Result<u8, CliError> {
    let Setup {
        config,
        overrides,
        mocks_dir,
        root,
        recorder,
    } = setup;
    let state = rustman_mock::load(&args.openapi, args.allow_remote_refs)
        .await?
        .with_overrides(overrides);
    let mut server = MockServer::bind(&config, Arc::new(state)).await?;
    if let Some(recorder) = recorder {
        server = server.with_recorder(recorder);
    }
    let addr = server.local_addr();
    let sink = Sink {
        jsonl: args.jsonl,
        history: history(args.history, root.as_deref())?,
        base_url: format!("http://{addr}"),
    };
    sink.spawn(server.subscribe());
    print_banner(&server.banner(), args.jsonl);
    if !addr.ip().is_loopback() {
        eprintln!("warning: {}", remote_warning(addr));
    }
    let reload = Reload {
        document: args.openapi.clone(),
        allow_remote_refs: args.allow_remote_refs,
        mocks_dir: Some(mocks_dir),
    };
    let _watcher = watch(server.reloader(reload))
        .inspect_err(|e| eprintln!("warning: changes will not be reloaded: {e}"))
        .ok();
    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(&cancel);
    Ok(stop_on(&cancel, server.spawn()).await)
}

/// Serve until `cancel` (Ctrl+C), then drain for [`DRAIN_GRACE`]; the
/// exit code.
async fn stop_on(cancel: &CancellationToken, handle: MockHandle) -> u8 {
    cancel.cancelled().await;
    handle.finish(DRAIN_GRACE).await;
    INTERRUPTED
}

/// The banner on stdout, or on stderr with `--jsonl` so stdout holds only
/// log entries.
fn print_banner(banner: &str, jsonl: bool) {
    if jsonl {
        eprintln!("{banner}");
    } else {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{banner}").and_then(|()| out.flush());
    }
}

/// `--history`: the workspace's history, else the ad-hoc one.
fn history(enabled: bool, root: Option<&Path>) -> Result<Option<HistoryStore>, CliError> {
    if !enabled {
        return Ok(None);
    }
    Ok(match root {
        Some(root) => {
            let ws = Workspace::load(root)?;
            history_store(false, Some(&ws), &ws.settings)
        }
        None => history_store(false, None, &Settings::ad_hoc()),
    })
}

/// Where log entries go. Entries are written on a thread of their own, so
/// a blocked stdout never holds up the server.
struct Sink {
    jsonl: bool,
    history: Option<HistoryStore>,
    base_url: String,
}

impl Sink {
    fn spawn(self, mut events: broadcast::Receiver<MockEvent>) {
        let spawned = std::thread::Builder::new()
            .name("mock-log".into())
            .spawn(move || {
                loop {
                    match events.blocking_recv() {
                        Ok(event) => self.write(&event),
                        Err(RecvError::Lagged(missed)) => {
                            eprintln!("warning: {missed} log entries not shown (output too slow)");
                        }
                        Err(RecvError::Closed) => return,
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("warning: requests will not be logged: {e}");
        }
    }

    fn write(&self, event: &MockEvent) {
        let MockEvent::Request(entry) = event else {
            if let Some(notice) = event.notice() {
                eprintln!("{notice}");
            }
            return;
        };
        if self.jsonl {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", entry.json_line()).and_then(|()| out.flush());
        } else {
            eprintln!("{}", entry.human_line());
        }
        if let Some(store) = &self.history
            && let Err(e) = store.record(&entry.history_entry(&self.base_url))
        {
            eprintln!("warning: history: {e}");
        }
    }
}

/// Flags first, then `[mock]`, then the defaults.
fn config(args: &ServeArgs, settings: &MockSettings) -> MockConfig {
    let config = settings.config();
    MockConfig {
        host: args.host.clone().unwrap_or(config.host),
        port: args.port.unwrap_or(config.port),
        allow_remote: args.allow_remote,
        seed: args.seed.unwrap_or(config.seed),
        prefer: !args.no_prefer,
        validate: !args.no_validate,
        enforce_security: args.enforce_security,
        stateful: args.stateful,
        cors: cors_choice(args),
        proxy_unmatched: args.proxy_unmatched.clone(),
        ..config
    }
}

/// `--cors` or `--no-cors`, else the default (on for loopback).
fn cors_choice(args: &ServeArgs) -> Option<bool> {
    match (args.cors, args.no_cors) {
        (_, true) => Some(false),
        (true, false) => Some(true),
        (false, false) => None,
    }
}

/// The workspace given with `--workspace`, or found from the current
/// directory; serving works without one.
fn workspace_root(explicit: Option<&Path>) -> Result<Option<PathBuf>, ConfigError> {
    match rustman_core::discovery::find_root(&current_dir()?, explicit) {
        Ok(root) => Ok(Some(root)),
        Err(ConfigError::NoWorkspace) if explicit.is_none() => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Ctrl+C path on every platform (the CLI test sends a real signal
    /// on unix only): the server stops and the exit code is 130.
    #[tokio::test]
    async fn a_cancel_stops_the_server_with_130() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/petstore.yaml");
        let state = rustman_mock::load(&fixture.to_string_lossy(), false)
            .await
            .unwrap();
        let config = MockConfig {
            port: 0,
            ..MockConfig::default()
        };
        let server = MockServer::bind(&config, Arc::new(state)).await.unwrap();
        let addr = server.local_addr();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(stop_on(&cancel, server.spawn()).await, INTERRUPTED);
        assert!(std::net::TcpStream::connect(addr).is_err());
    }
}
