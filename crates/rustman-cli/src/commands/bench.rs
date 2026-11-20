//! `rustman bench <path>... --target <origin>`: load test request files and
//! collections as virtual-user iterations (add-load-testing). The engine is
//! `rustman-bench`; this adapter reads the flags, the workspace and the
//! data file, prints the summary, writes the reports and maps the outcome
//! to an exit code (1 on a breached threshold, 130 on Ctrl+C).

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use rustman_bench::Live;
use rustman_bench::summary::render_outcome;
use rustman_bench::{
    Aborted, Bench, BenchError, BenchInput, BenchOutcome, DataFeed, DataMode, ExecutorKind,
    Exhaust, ProfileSpec, ReportContext, Safety, Stage, ThinkTime, Threshold, bench_report,
    interrupt_notice,
};
use rustman_core::ConfigError;
use rustman_core::bench::BenchSettings;
use rustman_core::data_rows::DataRows;
use rustman_core::run::{DataRun, Plan, plan, secret_columns};
use rustman_core::safety::Permission;
use rustman_http::runner::RunContext;
use rustman_report::{Report, ReportTarget};
use rustman_tui::bench_live::{self, Display, LiveHandle, LiveOptions};
use rustman_tui::bench_viz;
use rustman_tui::companion::Discovery;
use tokio_util::sync::CancellationToken;

use super::exec::{
    overrides, parse_duration, runtime, secret_providers, secret_store, stop_then_cancel_on_ctrl_c,
};
use super::run::scopes;
use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, INTERRUPTED, SUCCESS};

#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Request files or folders: one iteration runs them in collection
    /// order (default: the current directory)
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
    /// The origin under load, scheme://host[:port]; the only one requests
    /// may go to (required)
    #[arg(long, value_name = "URL")]
    target: Option<String>,
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// constant-vus (default), ramping-vus or constant-arrival-rate
    #[arg(long, value_name = "EXECUTOR")]
    executor: Option<ExecutorKind>,
    /// Virtual users of constant-vus (default 1)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    vus: Option<u32>,
    /// How long load runs: 30s (default), 2m, 500ms
    #[arg(long, value_name = "DURATION", value_parser = parse_duration,
          conflicts_with = "iterations")]
    duration: Option<Duration>,
    /// Iterations in total across every VU, instead of a duration
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    iterations: Option<u64>,
    /// Pause between a VU's iterations: 0 (default), 1s, or a uniform
    /// range such as 1s..3s
    #[arg(long, value_name = "TIME")]
    think_time: Option<ThinkTime>,
    /// A ramping-vus stage, <duration>:<vus>, as in 30s:20 (repeatable;
    /// replaces rustman.toml [[bench.stages]])
    #[arg(long = "stage", value_name = "DURATION:VUS")]
    stages: Vec<Stage>,
    /// Iterations started per second by constant-arrival-rate
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    rate: Option<u32>,
    /// VU pool of constant-arrival-rate (default 50)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_vus: Option<u32>,
    /// Feed each iteration a row of a CSV or JSON file as variables
    #[arg(long, value_name = "FILE")]
    data: Option<PathBuf>,
    /// sequential (default, one shared cursor), per-vu (VU n uses row n)
    /// or random
    #[arg(long, value_name = "MODE", requires = "data")]
    data_mode: Option<DataMode>,
    /// At the last row: recycle (default) or stop
    #[arg(long, value_name = "WHEN", requires = "data")]
    data_exhaust: Option<Exhaust>,
    /// Data columns whose values are secrets and are redacted in every
    /// output, besides those named like password, token, secret or api_key
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    secret_columns: Vec<String>,
    /// Also allow POST, PUT and PATCH
    #[arg(long)]
    allow_mutations: bool,
    /// Also allow DELETE (implies --allow-mutations; never against production)
    #[arg(long)]
    allow_destructive: bool,
    /// Confirm a production target by repeating its exact host
    #[arg(long, value_name = "HOST")]
    production: Option<String>,
    /// Total timeout per request, replacing settings.timeout_ms
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
    /// Fail the bench (exit 1) unless <metric><op><value> holds at the end,
    /// as in p95<300ms, error_rate<1% or file:login:p99<1s; a trailing !
    /// stops the bench as soon as it is breached (repeatable)
    #[arg(long = "threshold", value_name = "EXPR")]
    thresholds: Vec<Threshold>,
    /// Also write the report as json, html, markdown, junit or sarif, to a
    /// file or stdout, as in html=out/bench.html (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Stream per-second metrics to the viz companion (rustman-viz); the
    /// bench runs the same without it
    #[arg(long)]
    viz: bool,
}

impl BenchArgs {
    fn spec(&self) -> ProfileSpec {
        ProfileSpec {
            executor: self.executor.unwrap_or_default(),
            vus: self.vus,
            duration: self.duration,
            iterations: self.iterations,
            think: self.think_time.unwrap_or_default(),
            stages: self.stages.clone(),
            rate: self.rate,
            max_vus: self.max_vus,
        }
    }
}

pub fn run(args: &BenchArgs) -> Result<u8, CliError> {
    let Some(target) = args.target.as_deref() else {
        return Err(usage("bench: --target is required".into()).into());
    };
    rustman_report::render::one_stdout(&args.report).map_err(ConfigError::Usage)?;
    let ws = args.selection.workspace()?;
    let thresholds = thresholds(&ws.settings.bench, &args.thresholds)?;
    let env = args.selection.environment(&ws)?;
    let cwd = current_dir()?;
    let scopes = scopes(&ws, &cwd, &args.paths)?;
    let plan = plan(&ws, &scopes, &[]);
    if plan.is_empty() {
        return Err(usage("bench: no requests to run (check the paths)".into()).into());
    }
    let profile = args
        .spec()
        .or_workspace_stages(&ws.settings.bench)
        .build(ws.settings.bench.max_vus.get())?;
    let data = data_feed(args, &cwd, &plan)?;
    let store = secret_store(&ws.root)?;
    let (interrupt, cancel) = (CancellationToken::new(), CancellationToken::new());
    let reported = ReportContext {
        target,
        environment: env.map(|e| e.name.as_str()),
        profile: &profile,
    };
    let input = BenchInput {
        ctx: RunContext {
            root: &ws.root,
            settings: &ws.settings,
            environment: env,
            secrets: &store,
            in_workspace: true,
        },
        plan: &plan,
        profile: profile.clone(),
        safety: Safety {
            target,
            permission: Permission::from_flags(args.allow_mutations, args.allow_destructive),
            production: args.production.as_deref(),
        },
        overrides: overrides(&args.vars)?,
        timeout: args.timeout,
        insecure: args.insecure,
        data,
        thresholds,
        interrupt: interrupt.clone(),
        cancel: cancel.clone(),
        secret_providers: Some(secret_providers(&ws.root, &ws.settings)?),
    };
    let bench = Bench::prepare(input)?;
    let runtime = runtime()?;
    let live = bench.live();
    let display = Display::detect(on_stdout(&args.report));
    let shown = show_live(args, target, display, &live);
    // The dashboard shows the interruption in its title instead.
    let notice = (display != Display::Dashboard).then(interrupt_notice);
    let outcome = runtime.block_on(async {
        stop_then_cancel_on_ctrl_c(&interrupt, &cancel, notice);
        bench.run().await
    });
    shown.finish(&live, outcome.as_ref().ok());
    let outcome = outcome?;
    print_warnings(&store.take_warnings());
    print_warnings(&outcome.warnings);
    print_summary(&render_outcome(&outcome), &args.report);
    let report = bench_report(&outcome, &reported);
    super::report::write_all(&args.report, &Report::Bench(Box::new(report)))?;
    Ok(exit_code(&outcome))
}

/// Start the dashboard (stdout is a terminal) or the progress lines, and
/// the viz stream with `--viz`. Viz problems are a warning, shown once.
fn show_live(args: &BenchArgs, target: &str, display: Display, live: &Live) -> LiveHandle {
    let title = format!("rustman bench {target}");
    let viz = args
        .viz
        .then(|| bench_viz::start(&Discovery::from_env(), &title));
    let viz = viz.and_then(|started| started.map_err(|w| print_warnings(&[w])).ok());
    let lines: Box<dyn std::io::Write + Send> = if on_stdout(&args.report) {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    };
    let options = LiveOptions {
        display,
        title,
        lines,
        viz,
    };
    bench_live::spawn(live.clone(), options)
}

/// 130 when interrupted, 1 when a threshold failed (or aborted the run),
/// 0 otherwise.
fn exit_code(outcome: &BenchOutcome) -> u8 {
    if outcome.aborted == Some(Aborted::Interrupted) {
        INTERRUPTED
    } else if outcome.passed() {
        SUCCESS
    } else {
        ASSERTION
    }
}

/// Whether a report is written to stdout.
fn on_stdout(targets: &[ReportTarget]) -> bool {
    targets.iter().any(|t| t.path.is_none())
}

/// The summary on stdout, or on stderr when a report goes to stdout.
fn print_summary(text: &str, targets: &[ReportTarget]) {
    if on_stdout(targets) {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

/// The thresholds of `rustman.toml [bench.thresholds]`, then `--threshold`.
fn thresholds(settings: &BenchSettings, flags: &[Threshold]) -> Result<Vec<Threshold>, CliError> {
    let mut all = Vec::with_capacity(flags.len());
    for expression in settings.threshold_expressions() {
        let threshold = expression
            .parse()
            .map_err(|e| usage(format!("rustman.toml [bench.thresholds]: {e}")))?;
        all.push(threshold);
    }
    all.extend(flags.iter().cloned());
    Ok(all)
}

/// The rows of `--data`, with the secret columns of the plan's collections
/// and `--secret-columns`.
fn data_feed(
    args: &BenchArgs,
    cwd: &std::path::Path,
    plan: &Plan<'_>,
) -> Result<Option<DataFeed>, BenchError> {
    let Some(path) = &args.data else {
        return Ok(None);
    };
    let shown = format!("--data {}", path.display());
    let rows =
        DataRows::read(&cwd.join(path)).map_err(|e| BenchError::Usage(format!("{shown}: {e}")))?;
    let secret = secret_columns(plan, &args.secret_columns);
    let run = DataRun::new(rows, secret, None).map_err(BenchError::Usage)?;
    Ok(Some(DataFeed {
        run,
        mode: args.data_mode.unwrap_or_default(),
        exhaust: args.data_exhaust.unwrap_or_default(),
    }))
}

fn usage(message: String) -> ConfigError {
    ConfigError::Usage(message)
}
