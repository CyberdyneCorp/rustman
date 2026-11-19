//! `rustman mcp serve`: run Rustman as an MCP server for AI agents. The
//! server lives in `rustman_mcp::server`; this command finds the workspace,
//! checks the HTTP flags and wires stdio or a TCP listener to it.
//!
//! There are deliberately no flags that raise agent permissions: they come
//! only from `[agent]` in `rustman.toml` (agent-mcp-interface design D4).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_mcp::server::http::{self, DEFAULT_ADDR, HttpGuard, MCP_PATH};
use rustman_mcp::server::{Sessions, serve_stdio, token};
use tokio_util::sync::CancellationToken;

use super::EnvSelection;
use super::exec::secret_store;
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// Serve the workspace to AI agents over MCP (stdio by default)
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Serve Streamable HTTP at /mcp instead of stdio, on ADDR (default
    /// 127.0.0.1:8765)
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = DEFAULT_ADDR)]
    http: Option<String>,
    /// Allow --http on a non-loopback address
    #[arg(long, requires = "http")]
    allow_remote: bool,
    /// Read the HTTP bearer token from FILE (default: generate one and print
    /// it on stderr)
    #[arg(long, value_name = "FILE", requires = "http")]
    token_file: Option<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
}

pub fn run(args: &McpArgs) -> Result<u8, CliError> {
    match &args.command {
        McpCommand::Serve(args) => serve(args),
    }
}

fn serve(args: &ServeArgs) -> Result<u8, CliError> {
    let bind = args
        .http
        .as_deref()
        .map(|addr| http::bind_address(Some(addr), args.allow_remote))
        .transpose()
        .map_err(|e| ConfigError::Usage(e.to_string()))?;
    let workspace = args.selection.workspace()?;
    // An unknown `@preset` in `[jsonrpc] agent_read_only_methods` stops the
    // server before any client connects (exit code 2).
    rustman_mcp::server::tools::rpc::read_only_policy(&workspace)
        .map_err(|e| ConfigError::Usage(e.to_string()))?;
    let environment = args.selection.environment(&workspace)?.cloned();
    let secrets = secret_store(&workspace.root)?;
    let sessions = Sessions::new(workspace, secrets, environment);
    let runtime = super::exec::runtime()?;
    let served = match bind {
        None => runtime.block_on(serve_stdio(&sessions)),
        Some(addr) => runtime.block_on(serve_http(args, addr, sessions)),
    };
    served.map_err(ConfigError::Usage)?;
    Ok(crate::exit::SUCCESS)
}

async fn serve_http(
    args: &ServeArgs,
    addr: std::net::SocketAddr,
    sessions: Sessions,
) -> Result<(), String> {
    let token = match &args.token_file {
        Some(file) => token::read_file(file)?,
        None => {
            let token = token::generate();
            eprintln!("rustman mcp: bearer token (shown once): {token}");
            token
        }
    };
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    if args.allow_remote && !local.ip().is_loopback() {
        eprintln!(
            "warning: serving MCP on non-loopback address {local}; anyone who can reach it and holds the token can drive this workspace"
        );
    }
    eprintln!("rustman mcp: listening on http://{local}{MCP_PATH}");
    let origins = sessions.workspace().settings.agent.allowed_origins.clone();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    let guard = HttpGuard::new(token, origins);
    let any_host = http::accepts_any_host(local, args.allow_remote);
    let router = http::router(sessions, guard, any_host, cancel.clone());
    http::serve(listener, router, cancel)
        .await
        .map_err(|e| format!("the MCP HTTP server failed: {e}"))
}
