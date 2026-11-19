//! One module per subcommand.

pub mod auth;
pub mod completions;
pub mod cookies;
pub mod env;
pub mod exec;
pub mod explore;
pub mod export;
pub mod graphql;
pub mod grpc;
pub mod history;
pub mod import;
pub mod init;
pub mod mcp;
pub mod mcp_client;
pub mod mock;
pub mod realtime;
pub mod report;
pub mod rpc;
pub mod run;
pub mod secret;
pub mod send;
pub(crate) mod session;
pub mod snapshot;
pub mod tui;
pub mod webhook;

use std::path::{Path, PathBuf};

use clap::Args;
use rustman_core::environment::ENV_VAR;
use rustman_core::{ConfigError, Environment, Workspace};

/// Print non-fatal workspace problems on stderr.
pub fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

/// `--env` and `--workspace`, shared by commands that act on one environment.
#[derive(Debug, Args)]
pub struct EnvSelection {
    /// Environment (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    pub env: Option<String>,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
}

impl EnvSelection {
    /// The workspace, found from the current directory unless `--workspace`.
    pub fn workspace(&self) -> Result<Workspace, ConfigError> {
        let ws = Workspace::discover(&current_dir()?, self.workspace.as_deref())?;
        print_warnings(&ws.warnings);
        Ok(ws)
    }

    /// The active environment of `ws`, if any.
    pub fn environment<'a>(
        &self,
        ws: &'a Workspace,
    ) -> Result<Option<&'a Environment>, ConfigError> {
        let env_var = std::env::var(ENV_VAR).ok();
        ws.active_environment(self.env.as_deref(), env_var.as_deref())
    }
}

pub fn current_dir() -> Result<PathBuf, ConfigError> {
    std::env::current_dir().map_err(|e| ConfigError::Io {
        path: Path::new(".").to_path_buf(),
        source: e,
    })
}

/// How an environment is named in messages.
pub fn env_label(env: Option<&Environment>) -> String {
    env.map_or_else(
        || "no environment".to_owned(),
        |e| format!("environment '{}'", e.name),
    )
}
