//! `rustman explore`: plan, run, resume, inspect and export explorations of
//! an API from its OpenAPI document. The work happens in
//! `rustman_explore::explore`; these commands wire flags, the workspace and
//! the terminal to it.

mod export;
mod plan;
mod run;
mod status;

use std::collections::BTreeSet;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;

use clap::{Args, Subcommand};
use rustman_core::safety::{Limits, Origin, SafetyPolicy};
use rustman_core::settings::Settings as WorkspaceSettings;
use rustman_core::{ConfigError, Environment, Workspace};
use rustman_explore::explore::plan::{DEFAULT_FUZZ_CASES, Family};
use rustman_explore::explore::session::Settings;
use rustman_explore::explore::setup::{self, plan_options};

use super::{EnvSelection, current_dir};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ExploreArgs {
    #[command(subcommand)]
    command: ExploreCommand,
}

#[derive(Debug, Subcommand)]
enum ExploreCommand {
    /// Discover and plan probes without sending any request (a dry run)
    Plan(plan::PlanArgs),
    /// Discover, plan, send the probes and report the findings
    Run(run::RunArgs),
    /// Continue an interrupted exploration with the probes not yet run
    Resume(run::ResumeArgs),
    /// Show the phase, progress and findings of an exploration
    Status(status::StatusArgs),
    /// Write findings as regression request files for `rustman run`
    Export(export::ExportArgs),
}

pub fn run(args: &ExploreArgs) -> Result<u8, CliError> {
    match &args.command {
        ExploreCommand::Plan(a) => plan::plan_command(a).map(|()| crate::exit::SUCCESS),
        ExploreCommand::Run(a) => run::run_command(a),
        ExploreCommand::Resume(a) => run::resume_command(a),
        ExploreCommand::Status(a) => status::status_command(a).map(|()| crate::exit::SUCCESS),
        ExploreCommand::Export(a) => export::export_command(a).map(|()| crate::exit::SUCCESS),
    }
}

/// What every exploration command reads: the document, the target, the
/// probe selection and the safety flags.
#[derive(Debug, Args)]
pub struct ExploreFlags {
    /// The OpenAPI document: a file path or an http(s) URL
    #[arg(long, value_name = "SOURCE")]
    spec: String,
    /// The base URL probes are sent to; its origin is the only one allowed
    #[arg(long, value_name = "URL")]
    target: String,
    /// Probe families, comma-separated (default: all but bola): conformance,
    /// auth, negative-input, method-not-allowed, lifecycle, bola, fuzz,
    /// undocumented-status, error-leakage, security-headers
    #[arg(long, value_name = "LIST", value_parser = Family::parse_list)]
    probes: Option<BTreeSet<Family>>,
    /// Only operations matching a glob over operationId, path or
    /// "METHOD /path" (repeatable)
    #[arg(long = "operations", value_name = "GLOB")]
    operations: Vec<String>,
    /// Seed of the fuzz cases (default: random, printed with the plan)
    #[arg(long, value_name = "U64")]
    seed: Option<u64>,
    /// At most this many fuzz cases per operation
    #[arg(long, value_name = "N", default_value_t = DEFAULT_FUZZ_CASES)]
    fuzz_cases: usize,
    /// Also allow POST, PUT and PATCH
    #[arg(long)]
    allow_mutations: bool,
    /// Also allow DELETE (implies --allow-mutations; never against production)
    #[arg(long)]
    allow_destructive: bool,
    /// Allow another origin, scheme://host[:port] (repeatable)
    #[arg(long = "allow-host", value_name = "ORIGIN", value_parser = Origin::parse)]
    allow_hosts: Vec<Origin>,
    /// Confirm a production target by repeating its exact host
    #[arg(long, value_name = "HOST")]
    production: Option<String>,
    /// The environment of a second identity, for bola probes
    #[arg(long, value_name = "ENV")]
    identity_b: Option<String>,
    /// Fetch $refs to other http(s) documents instead of refusing them
    #[arg(long)]
    allow_remote_refs: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

impl ExploreFlags {
    /// The settings a resume repeats, with these `limits` and `fail_on`.
    fn settings(
        &self,
        limits: Limits,
        fail_on: rustman_explore::explore::rules::Severity,
    ) -> Settings {
        Settings {
            probes: self
                .probes
                .clone()
                .unwrap_or_else(Family::defaults)
                .into_iter()
                .collect(),
            operations: self.operations.clone(),
            seed: self.seed.unwrap_or_else(random_seed),
            fuzz_cases: self.fuzz_cases,
            allow_mutations: self.allow_mutations || self.allow_destructive,
            allow_destructive: self.allow_destructive,
            allow_hosts: self.allow_hosts.iter().map(ToString::to_string).collect(),
            production: self.production.clone(),
            allow_remote_refs: self.allow_remote_refs,
            rate: limits.rate.get(),
            concurrency: limits.concurrency.get(),
            max_probes: limits.max_probes,
            max_duration_ms: u64::try_from(limits.max_duration.as_millis()).unwrap_or(u64::MAX),
            fail_on,
        }
    }
}

/// `--workspace` alone, for commands that act on recorded explorations.
#[derive(Debug, Args)]
pub struct RootArgs {
    /// Workspace directory (default: search from the current directory
    /// upwards, else the current directory)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

impl RootArgs {
    /// Where `.rustman/explorations` is: the workspace root, else (without
    /// `--workspace`) the current directory.
    fn root(&self) -> Result<PathBuf, CliError> {
        Ok(self.root_and_workspace()?.0)
    }

    /// The root, with its workspace when there is one.
    fn root_and_workspace(&self) -> Result<(PathBuf, Option<Workspace>), CliError> {
        let cwd = current_dir()?;
        match Workspace::discover(&cwd, self.workspace.as_deref()) {
            Ok(ws) => {
                super::print_warnings(&ws.warnings);
                Ok((ws.root.clone(), Some(ws)))
            }
            Err(ConfigError::NoWorkspace) if self.workspace.is_none() => Ok((cwd, None)),
            Err(e) => Err(e.into()),
        }
    }
}

/// The workspace, when there is one; without `--workspace`, none is fine.
fn optional_workspace(selection: &EnvSelection) -> Result<Option<Workspace>, CliError> {
    match selection.workspace() {
        Ok(ws) => Ok(Some(ws)),
        Err(ConfigError::NoWorkspace)
            if selection.workspace.is_none() && selection.env.is_none() =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// The `--identity-b` environment, which must exist.
fn identity_b<'a>(
    workspace: Option<&'a Workspace>,
    name: &str,
) -> Result<&'a Environment, CliError> {
    let ws = workspace.ok_or_else(|| {
        ConfigError::Usage(format!(
            "--identity-b {name} needs a workspace with that environment"
        ))
    })?;
    let env = ws.active_environment(Some(name), None)?;
    Ok(env.ok_or(ConfigError::NoActiveEnvironment)?)
}

/// The safety policy of `settings`, `[safety]` and the active environment.
fn policy(
    target: &str,
    settings: &Settings,
    workspace: &WorkspaceSettings,
    environment: Option<&Environment>,
) -> Result<SafetyPolicy, CliError> {
    setup::policy(target, settings, workspace, environment).map_err(|e| match e {
        setup::PolicySetupError::InvalidHost(message) => ConfigError::Usage(message).into(),
        setup::PolicySetupError::Policy(e) => e.into(),
    })
}

/// A seed from the standard library's per-process random hasher keys.
fn random_seed() -> u64 {
    RandomState::new().hash_one(std::time::SystemTime::now())
}
