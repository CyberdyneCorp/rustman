//! `rustman explore run` and `explore resume`: the four phases (discover,
//! plan, execute, report) with live progress on stderr, Ctrl+C handling and
//! the exit codes 0, 1 (a finding at or above `--fail-on`), 2, 4 and 130.

use std::io::Write;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, Instant};

use clap::Args;
use rustman_core::safety::{
    DEFAULT_MAX_DURATION, DEFAULT_MAX_PROBES, Governor, Limits, SafetyPolicy,
};
use rustman_core::settings::Settings as WorkspaceSettings;
use rustman_core::{ConfigError, Environment, Workspace};
use rustman_explore::explore::drive::{Drive, Finished, drive};
use rustman_explore::explore::exec::{
    Context, Executor, Identity, exploration_export_warning, exploration_send_options,
};
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::journal::{self, requests_sent};
use rustman_explore::explore::plan::{self, Disposition, Plan};
use rustman_explore::explore::report::ExplorationReport;
use rustman_explore::explore::rules::Severity;
use rustman_explore::explore::session::{Progress, RunState, Session};
use rustman_explore::explore::setup::{self, NewExploration};
use rustman_explore::store::sha256;
use rustman_explore::{ApiModel, ImportOptions, Imported, import};
use rustman_http::GovernedSender;
use rustman_report::{Report, ReportTarget};
use tokio_util::sync::CancellationToken;

use super::{ExploreFlags, RootArgs, identity_b, optional_workspace, plan_options, policy};
use crate::commands::exec::{cancel_on_ctrl_c, parse_duration, runtime, secret_store};
use crate::commands::{current_dir, print_warnings};
use crate::exit::{self, CliError};

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    flags: ExploreFlags,
    /// Requests per second at the start (default: [safety].rate, 10)
    #[arg(long, value_name = "N")]
    rate: Option<NonZeroU32>,
    /// Probes in flight at once (default: [safety].concurrency, 4)
    #[arg(long, value_name = "N")]
    concurrency: Option<NonZeroU32>,
    /// Probe budget: at most this many requests
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_PROBES)]
    max_probes: u64,
    /// Time budget: 90s, 15m, or milliseconds (default: 15m)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    max_duration: Option<Duration>,
    /// Exit 1 when a finding is at or above this severity: critical, high,
    /// medium, low or info
    #[arg(long, value_name = "SEVERITY", default_value = "high")]
    fail_on: Severity,
    /// Also render the report as json, junit, markdown, html or sarif,
    /// written to a file or stdout, as in sarif=out/r.sarif (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// The exploration id, exp-<date>-<hex>
    id: String,
    #[command(flatten)]
    root: RootArgs,
}

/// Everything the execute and report phases need.
struct Execution<'a> {
    session: Session,
    workspace: Option<&'a Workspace>,
    root: &'a Path,
    environment: Option<&'a Environment>,
    identity_b: Option<&'a Environment>,
    policy: SafetyPolicy,
    limits: Limits,
    model: &'a ApiModel,
    plan: &'a Plan,
    /// `--report` renderings of the finished report.
    targets: &'a [ReportTarget],
}

pub fn run_command(args: &RunArgs) -> Result<u8, CliError> {
    rustman_report::render::one_stdout(&args.report).map_err(usage)?;
    let flags = &args.flags;
    let workspace = optional_workspace(&flags.selection)?;
    let root = match &workspace {
        Some(ws) => ws.root.clone(),
        None => current_dir()?,
    };
    let ws_settings = workspace_settings(workspace.as_ref());
    let environment = match &workspace {
        Some(ws) => flags.selection.environment(ws)?,
        None => None,
    };
    let second = match &flags.identity_b {
        Some(name) => Some(identity_b(workspace.as_ref(), name)?),
        None => None,
    };
    let limits = Limits {
        rate: args.rate.unwrap_or(ws_settings.safety.rate),
        concurrency: args.concurrency.unwrap_or(ws_settings.safety.concurrency),
        max_probes: args.max_probes,
        max_duration: args.max_duration.unwrap_or(DEFAULT_MAX_DURATION),
    };
    let settings = flags.settings(limits, args.fail_on);
    let policy = policy(&flags.target, &settings, &ws_settings, environment)?;
    let runtime = runtime()?;
    let imported = load(&runtime, &flags.spec, settings.allow_remote_refs)?;
    let model = &imported.model;
    let started = Instant::now();
    let graph = Graph::build(model, &ws_settings.explore.producers);
    print_warnings(&graph.warnings);
    progress_line(&format!(
        "discover: {} operations, {} producer edges, {} unresolved parameters",
        model.operations.len(),
        graph.edges.len(),
        graph.unresolved.len()
    ));
    let options = plan_options(
        &settings,
        &policy,
        &ws_settings,
        environment,
        second.is_some(),
    );
    let planned = plan::plan(model, &graph, &options);
    progress_line(&plan_summary(&planned));
    let state = setup::new_state(NewExploration {
        spec: &flags.spec,
        imported: &imported,
        target: &flags.target,
        environment,
        identity_b: flags.identity_b.clone(),
        settings,
        graph,
    });
    let session = Session::create(&root, state, &planned)?;
    progress_line(&format!(
        "exploration {} started ({} ms to discover and plan)",
        session.state.id,
        started.elapsed().as_millis()
    ));
    execute(
        &runtime,
        Execution {
            session,
            workspace: workspace.as_ref(),
            root: &root,
            environment,
            identity_b: second,
            policy,
            limits,
            model,
            plan: &planned,
            targets: &args.report,
        },
    )
}

pub fn resume_command(args: &ResumeArgs) -> Result<u8, CliError> {
    let (root, workspace) = args.root.root_and_workspace()?;
    let session = Session::open(&root, &args.id)?;
    let state = &session.state;
    if state.state == RunState::Completed {
        return Err(usage(format!(
            "exploration {} is already complete; its report is {}",
            state.id,
            session.report_path().display()
        )));
    }
    let ws_settings = workspace_settings(workspace.as_ref());
    let environment = named_environment(workspace.as_ref(), state.environment.as_deref())?;
    let second = match &state.identity_b {
        Some(name) => Some(identity_b(workspace.as_ref(), name)?),
        None => None,
    };
    let policy = policy(&state.target, &state.settings, &ws_settings, environment)?;
    let runtime = runtime()?;
    let imported = load(&runtime, &state.spec, state.settings.allow_remote_refs)?;
    if sha256(&imported.text) != state.document_sha256 {
        return Err(usage(format!(
            "the document {} changed since exploration {} started; start a new one with `rustman explore run`",
            state.spec, state.id
        )));
    }
    let planned = session.plan()?;
    let entries = journal::read(&session.journal_path()).map_err(|e| ConfigError::Io {
        path: session.journal_path(),
        source: e,
    })?;
    let limits = setup::remaining(&session.state, requests_sent(&entries));
    progress_line(&format!(
        "resuming exploration {}: {} of {} probes left",
        state.id,
        planned.probes.len() - journal::outcomes(&entries).count(),
        planned.probes.len()
    ));
    execute(
        &runtime,
        Execution {
            session,
            workspace: workspace.as_ref(),
            root: &root,
            environment,
            identity_b: second,
            policy,
            limits,
            model: &imported.model,
            plan: &planned,
            targets: &[],
        },
    )
}

/// Run the execute and report phases and map the end to an exit code.
fn execute(runtime: &tokio::runtime::Runtime, execution: Execution<'_>) -> Result<u8, CliError> {
    let Execution {
        mut session,
        workspace,
        root,
        environment,
        identity_b,
        policy,
        limits,
        model,
        plan,
        targets,
    } = execution;
    let ws_settings = workspace_settings(workspace);
    let secrets = secret_store(root)?;
    let sender = GovernedSender::new(policy, Governor::new(limits));
    if let Some(warning) = exploration_export_warning(&ws_settings, environment) {
        eprintln!("warning: {warning}");
    }
    let graph = session.state.graph.clone();
    let target = session.state.target.clone();
    let context = Context {
        model,
        graph: &graph,
        target: &target,
        variables: &ws_settings.variables,
        identity: Identity {
            environment,
            secrets: &secrets,
        },
        identity_b: identity_b.map(|environment| Identity {
            environment: Some(environment),
            secrets: &secrets,
        }),
        scans: &plan.scans,
        disabled_rules: &ws_settings.explore.disabled_rules,
        send: exploration_send_options(&ws_settings, environment, &secrets).map_err(usage)?,
    };
    let executor = Executor::new(&sender, context);
    let cancel = CancellationToken::new();
    let mut printer = Printer::default();
    let fail_on = session.state.settings.fail_on;
    let finished = runtime.block_on(async {
        cancel_on_ctrl_c(&cancel);
        let options = Drive {
            plan,
            model,
            concurrency: usize::try_from(limits.concurrency.get()).unwrap_or(1),
            cancel: cancel.clone(),
            progress: &mut |p, elapsed| printer.print(p, elapsed),
        };
        drive(&mut session, &executor, options).await
    })?;
    match finished {
        Finished::Interrupted(progress) => {
            eprintln!(
                "interrupted after {}/{} probes; continue with `rustman explore resume {}`",
                progress.done, progress.total, session.state.id
            );
            Ok(exit::INTERRUPTED)
        }
        Finished::Completed(report) => {
            let text = summary(&report, &session.report_path());
            // A report on stdout keeps it machine-readable.
            if targets.iter().any(|t| t.path.is_none()) {
                eprint!("{text}");
            } else {
                print!("{text}");
            }
            let failing = report.failing(fail_on);
            crate::commands::report::write_all(targets, &Report::Exploration(report))?;
            Ok(if failing > 0 {
                exit::ASSERTION
            } else {
                exit::SUCCESS
            })
        }
    }
}

fn load(
    runtime: &tokio::runtime::Runtime,
    spec: &str,
    allow_remote_refs: bool,
) -> Result<Imported, CliError> {
    let options = ImportOptions {
        allow_remote_refs,
        ..ImportOptions::default()
    };
    let imported = runtime.block_on(import(spec, &options))?;
    for warning in &imported.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(imported)
}

fn workspace_settings(workspace: Option<&Workspace>) -> WorkspaceSettings {
    workspace.map_or_else(WorkspaceSettings::ad_hoc, |ws| ws.settings.clone())
}

fn named_environment<'a>(
    workspace: Option<&'a Workspace>,
    name: Option<&str>,
) -> Result<Option<&'a Environment>, CliError> {
    match (workspace, name) {
        (Some(ws), Some(name)) => Ok(ws.active_environment(Some(name), None)?),
        (None, Some(name)) => Err(usage(format!(
            "the exploration used environment '{name}', but there is no workspace"
        ))),
        (_, None) => Ok(None),
    }
}

fn usage(message: String) -> CliError {
    ConfigError::Usage(message).into()
}

fn plan_summary(plan: &Plan) -> String {
    let count =
        |f: fn(&Disposition) -> bool| plan.probes.iter().filter(|p| f(&p.disposition)).count();
    format!(
        "plan: {} probes ({} allowed, {} blocked, {} skipped), seed {}",
        plan.probes.len(),
        count(|d| *d == Disposition::Allowed),
        count(|d| matches!(d, Disposition::Blocked(_))),
        count(|d| matches!(d, Disposition::Skipped(_))),
        plan.seed
    )
}

fn progress_line(line: &str) {
    eprintln!("{line}");
}

/// Prints the execute phase's progress at most once a second, and always
/// for the last probe.
#[derive(Default)]
struct Printer {
    last: Option<Instant>,
}

impl Printer {
    fn print(&mut self, progress: &Progress, elapsed: Duration) {
        let due = self
            .last
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if !due && progress.done < progress.total {
            return;
        }
        self.last = Some(Instant::now());
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "execute: {}/{} probes, {} findings, {:.1}s",
            progress.done,
            progress.total,
            progress.findings.total(),
            elapsed.as_secs_f64()
        );
    }
}

/// The text `explore run` prints when it completes.
fn summary(report: &ExplorationReport, path: &Path) -> String {
    use std::fmt::Write as _;
    let s = &report.summary;
    let p = &s.probes;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "exploration {} completed (seed {})",
        report.id, report.seed
    );
    let _ = writeln!(
        out,
        "probes: {} ({} passed, {} finding, {} blocked, {} skipped, {} not run, {} failed)",
        p.total, p.passed, p.finding, p.blocked, p.skipped, p.not_run, p.failed
    );
    let _ = writeln!(out, "findings: {}", s.findings);
    for f in &report.findings {
        let _ = writeln!(
            out,
            "  {}  {:<8}  {}  {}  ({}x)",
            f.id, f.severity, f.rule, f.operation, f.occurrences
        );
    }
    let c = &report.coverage;
    let _ = writeln!(
        out,
        "coverage: operations {}/{} ({}%), statuses {}/{} ({}%)",
        c.operations.covered,
        c.operations.total,
        c.operations.percent,
        c.statuses.covered,
        c.statuses.total,
        c.statuses.percent
    );
    if !report.left_behind.is_empty() {
        let _ = writeln!(out, "left behind: {} resources", report.left_behind.len());
        for r in &report.left_behind {
            let _ = writeln!(out, "  {}", r.url);
        }
    }
    let _ = writeln!(out, "report: {}", path.display());
    out
}
