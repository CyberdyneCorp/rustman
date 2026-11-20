//! `rustman monitor`: scheduled monitors (add-scheduled-monitors). The
//! monitor files, runs, policy, store, alerts, daemon, recipes and views
//! live in `rustman-monitor`; this module parses the flags and prints.

use std::io::Write;
use std::path::PathBuf;

mod alerts;
mod daemon;
mod views;

use clap::{Args, Subcommand, ValueEnum};
use rustman_core::{ConfigError, SecretStore, Workspace};
use rustman_http::HttpEngine;
use rustman_monitor::cycle::CycleContext;
use rustman_monitor::notify::{self, SystemDesktop};
use rustman_monitor::recipe::{self, Target};
use rustman_monitor::run::exit;
use rustman_monitor::{Monitor, MonitorError, MonitorRun, StoreError};
use tokio_util::sync::CancellationToken;

use super::exec::{cancel_on_ctrl_c, runtime, secret_store};
use super::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    command: MonitorCommand,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR", global = true)]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum MonitorCommand {
    /// Run monitors once, store the results and exit 0 (pass), 1 (an
    /// assertion failed), 3 (a request errored) or 4 (blocked): for cron,
    /// systemd timers, launchd and CI schedules
    Run(RunArgs),
    /// Run every monitor of the workspace on its schedule until stopped
    Daemon,
    /// Show each monitor's last result, uptime and latency trend
    Status(StatusArgs),
    /// List the monitors with their schedule and environment
    List(ListArgs),
    /// List the stored results of a monitor, newest first
    History(HistoryArgs),
    /// Send a test notification through every channel of a monitor
    TestAlert(NameArg),
    /// Print a scheduler configuration that runs a monitor
    ScheduleRecipe(RecipeArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The monitor to run (its name)
    #[arg(
        value_name = "NAME",
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    name: Option<String>,
    /// Run every monitor of the workspace, one after the other
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Print JSON instead of a table
    #[arg(long)]
    json: bool,
    /// Open the trend view in the viz companion (rustman-viz); falls back
    /// to the text status without it
    #[arg(long)]
    viz: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Print a JSON array instead of a table
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(super) struct HistoryArgs {
    /// The monitor (its name)
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Only results this recent: 30m, 24h, 7d
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,
    /// Only results with this outcome: pass, fail, error or skipped
    #[arg(long, value_name = "OUTCOME")]
    pub outcome: Option<String>,
    /// Print JSON lines instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
struct NameArg {
    /// The monitor (its name)
    #[arg(value_name = "NAME")]
    name: String,
}

#[derive(Debug, Args)]
struct RecipeArgs {
    /// The monitor (its name)
    #[arg(value_name = "NAME")]
    name: String,
    /// The scheduler to configure
    #[arg(long = "for", value_name = "SCHEDULER", value_enum)]
    target: Scheduler,
    /// Write the configuration to this file instead of printing it
    #[arg(long, value_name = "PATH")]
    write: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Scheduler {
    Cron,
    Systemd,
    Launchd,
    GithubActions,
}

pub fn run(args: &MonitorArgs) -> Result<u8, CliError> {
    let ws = Workspace::discover(&current_dir()?, args.workspace.as_deref())?;
    print_warnings(&ws.warnings);
    let monitors = rustman_monitor::load_all(&ws.root)?;
    match &args.command {
        MonitorCommand::Run(run) => run_once(&ws, &monitors, run),
        MonitorCommand::List(list) => {
            print_list(&monitors, list.json);
            Ok(SUCCESS)
        }
        MonitorCommand::Daemon => daemon::run(&ws),
        MonitorCommand::Status(status) => views::status(&ws, &monitors, status.json, status.viz),
        MonitorCommand::History(history) => views::history(&ws, &monitors, history),
        MonitorCommand::TestAlert(test) => alerts::test_alert(&ws, &monitors, &test.name),
        MonitorCommand::ScheduleRecipe(recipe) => schedule_recipe(&ws, &monitors, recipe),
    }
}

/// What a cycle needs, owned by the command.
struct Session {
    secrets: SecretStore,
    engine: HttpEngine,
    retention: std::time::Duration,
}

impl Session {
    fn open(ws: &Workspace) -> Result<Self, CliError> {
        Ok(Self {
            secrets: secret_store(&ws.root)?,
            engine: HttpEngine::new(),
            retention: rustman_monitor::retention(&ws.settings.monitors)?,
        })
    }

    fn context<'a>(&'a self, ws: &'a Workspace) -> CycleContext<'a> {
        CycleContext {
            ws,
            secrets: &self.secrets,
            desktop: &SystemDesktop,
            engine: &self.engine,
            retention: self.retention,
            notify_timeout: notify::TIMEOUT,
        }
    }
}

/// `rustman monitor run <name>|--all`: each run is stored, its alerts
/// evaluated and sent, then the exit code of the worst run is returned.
fn run_once(ws: &Workspace, monitors: &[Monitor], args: &RunArgs) -> Result<u8, CliError> {
    let selected: Vec<&Monitor> = match &args.name {
        Some(name) => vec![rustman_monitor::find(monitors, name)?],
        None if monitors.is_empty() => {
            return Err(ConfigError::Usage(
                "no monitors in this workspace (add monitors/<name>.rman-monitor.yaml)".into(),
            )
            .into());
        }
        None => monitors.iter().collect(),
    };
    let session = Session::open(ws)?;
    let ctx = session.context(ws);
    let rt = runtime()?;
    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(&cancel);
    let mut code = SUCCESS;
    for monitor in selected {
        let cycle = rt
            .block_on(ctx.run(monitor, cancel.clone()))
            .map_err(monitor_error)?;
        print_run(&monitor.name, &cycle.run);
        print_warnings(&cycle.warnings);
        let mut out = anstream::stdout().lock();
        for line in alerts::lines(&cycle.alerts) {
            let _ = writeln!(out, "{line}");
        }
        code = worst(code, cycle.run.exit_code());
    }
    print_warnings(&session.secrets.take_warnings());
    Ok(code)
}

/// `api-health: fail in 123 ms`, then one indented line per failure.
fn print_run(name: &str, run: &MonitorRun) {
    let result = &run.result;
    let mut out = anstream::stdout().lock();
    let _ = writeln!(
        out,
        "{name}: {} in {:.0} ms",
        result.outcome, result.duration_ms
    );
    for line in result.message.iter().chain(&result.failures) {
        let _ = writeln!(out, "  {line}");
    }
}

/// The more severe of two exit codes: interrupted, blocked, errored,
/// failed, passed.
fn worst(a: u8, b: u8) -> u8 {
    let rank = |code| match code {
        exit::INTERRUPTED => 4,
        exit::BLOCKED => 3,
        exit::ERROR => 2,
        exit::FAIL => 1,
        _ => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}

fn monitor_error(err: MonitorError) -> CliError {
    match err {
        MonitorError::Config(e) => e.into(),
        MonitorError::Policy(e) => e.into(),
        MonitorError::Request(e) => e.into(),
        MonitorError::Store(e) => store_error(e),
    }
}

fn store_error(err: StoreError) -> CliError {
    match err {
        StoreError::Io { path, source } => ConfigError::Io { path, source }.into(),
        other @ StoreError::InvalidState { .. } => ConfigError::Usage(other.to_string()).into(),
    }
}

/// `rustman monitor schedule-recipe <name> --for <scheduler> [--write]`.
fn schedule_recipe(
    ws: &Workspace,
    monitors: &[Monitor],
    args: &RecipeArgs,
) -> Result<u8, CliError> {
    let monitor = rustman_monitor::find(monitors, &args.name)?;
    let target = match args.target {
        Scheduler::Cron => Target::Cron,
        Scheduler::Systemd => Target::Systemd,
        Scheduler::Launchd => Target::Launchd,
        Scheduler::GithubActions => Target::GithubActions,
    };
    let root = std::fs::canonicalize(&ws.root).unwrap_or_else(|_| ws.root.clone());
    let program = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "rustman".to_owned());
    let text = recipe::recipe(monitor, &root, &program, target)
        .map_err(|e| ConfigError::Usage(format!("monitor '{}': {e}", monitor.name)))?;
    match &args.write {
        Some(path) => {
            std::fs::write(path, &text).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            println!("wrote {}", path.display());
        }
        None => print!("{text}"),
    }
    Ok(SUCCESS)
}

/// `NAME  SCHEDULE  ENVIRONMENT  TARGETS`, or a JSON array.
fn print_list(monitors: &[Monitor], json: bool) {
    let mut out = std::io::stdout().lock();
    if json {
        let entries: Vec<serde_json::Value> = monitors.iter().map(list_entry).collect();
        let text = serde_json::to_string_pretty(&entries).expect("entries serialize");
        let _ = writeln!(out, "{text}");
        return;
    }
    if monitors.is_empty() {
        let _ = writeln!(out, "no monitors (add monitors/<name>.rman-monitor.yaml)");
        return;
    }
    let rows: Vec<[String; 4]> = monitors
        .iter()
        .map(|m| {
            [
                m.name.clone(),
                m.schedule.to_string(),
                m.environment.clone(),
                m.targets.join(", "),
            ]
        })
        .collect();
    let header = ["NAME", "SCHEDULE", "ENVIRONMENT", "TARGETS"].map(str::to_owned);
    let widths: Vec<usize> = (0..3)
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let _ = writeln!(
            out,
            "{:<w0$}  {:<w1$}  {:<w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2]
        );
    }
}

fn list_entry(monitor: &Monitor) -> serde_json::Value {
    serde_json::json!({
        "name": monitor.name,
        "file": monitor.file.to_string_lossy().replace('\\', "/"),
        "schedule": monitor.schedule.to_string(),
        "environment": monitor.environment,
        "targets": monitor.targets,
        "timeout_ms": u64::try_from(monitor.timeout.as_millis()).unwrap_or(u64::MAX),
        "allow_mutations": monitor.allow_mutations,
    })
}
