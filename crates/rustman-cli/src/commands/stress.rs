//! `rustman stress <mode> <path>... --target <origin>`: find where a backend
//! breaks, how, and whether it recovers (add-stress-testing). The engine is
//! `rustman_bench::stress`; this adapter reads the flags and the workspace,
//! shows the run live (a dashboard on a terminal, a line per step
//! otherwise, `--viz`), prints the summary, keeps the report as
//! `.rustman/stress/<id>/report.json`, writes `--report` renderings and
//! maps the outcome to an exit code: 0, 1 (a finding at or above
//! `--fail-on`, or `max_sustainable_rps` below `--require-rps`), 2 (usage
//! or a cap), 4 (a safety refusal) and 130 (Ctrl+C).
//!
//! `rustman stress export <id>` writes a run's findings as regression
//! request files.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use rustman_bench::stress::export::{self, ExportError};
use rustman_bench::stress::report::{StressReportContext, stress_report};
use rustman_bench::stress::run::Tuning;
use rustman_bench::stress::summary::render;
use rustman_bench::stress::{
    BreakpointSpec, FlatSpec, ModeKind, ModeSpec, Slo, SoakSpec, SpikeSpec, Stress, StressInput,
    StressOutcome,
};
use rustman_core::ConfigError;
use rustman_core::safety::Permission;
use rustman_core::stress::parse_stress_duration;
use rustman_report::exploration::Severity;
use rustman_report::{Report, ReportTarget};
use rustman_tui::companion::Discovery;
use rustman_tui::stress_live::{self, StressLiveHandle, StressLiveOptions};
use tokio_util::sync::CancellationToken;

use super::exec::{overrides, runtime, secret_store, stop_then_cancel_on_ctrl_c};
use super::run::scopes;
use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, INTERRUPTED, SUCCESS};

/// A stress mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Step the rate up until an SLO breaks, then narrow down the last
    /// sustainable rate
    Breakpoint,
    /// Baseline, a sudden multiple of it, baseline again: errors and
    /// recovery time
    Spike,
    /// A steady rate for a long time: latency and error drift
    Soak,
    /// A steady rate with a share of mutated requests
    Fuzz,
}

impl From<Mode> for ModeKind {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Breakpoint => Self::Breakpoint,
            Mode::Spike => Self::Spike,
            Mode::Soak => Self::Soak,
            Mode::Fuzz => Self::Fuzz,
        }
    }
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct StressArgs {
    #[command(subcommand)]
    command: Option<StressCommand>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Debug, Subcommand)]
enum StressCommand {
    /// Write findings of a stress run as regression request files for
    /// `rustman run`
    Export(ExportArgs),
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// The stress run id, str-<date>-<hex> (printed at the end of the run)
    id: String,
    /// Finding ids from the report (f001,f002) or `all`
    #[arg(long, value_name = "IDS", value_delimiter = ',', required = true)]
    findings: Vec<String>,
    /// The folder to write the files into (created if needed)
    #[arg(long, value_name = "DIR")]
    into: PathBuf,
    #[command(flatten)]
    selection: EnvSelection,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// breakpoint, spike, soak or fuzz
    #[arg(value_name = "MODE", required = true)]
    mode: Option<Mode>,
    /// Request files or folders: one iteration runs them in collection
    /// order (default: the current directory)
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
    /// The origin under stress, scheme://host[:port]; it must be listed in
    /// rustman.toml [stress] targets (required)
    #[arg(long, value_name = "URL")]
    target: Option<String>,
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// An SLO judged over each 5 s window, such as p95<300ms, error_rate<1%
    /// or "file:checkout p99<800ms" (repeatable; after rustman.toml
    /// [stress.slo]; default p95<1s and error_rate<5% for breakpoint and
    /// spike)
    #[arg(long = "slo", value_name = "EXPR")]
    slos: Vec<Slo>,
    /// breakpoint: the first rate, in iterations per second (default 10)
    #[arg(long, value_name = "N")]
    start_rps: Option<f64>,
    /// breakpoint: the factor between steps (default 1.5)
    #[arg(long, value_name = "X")]
    step_factor: Option<f64>,
    /// breakpoint: how long each step lasts (default 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    step_duration: Option<Duration>,
    /// breakpoint: bisection steps after the first break (default 3)
    #[arg(long, value_name = "N")]
    refine: Option<u32>,
    /// spike: the rate before and after the spike (default 10)
    #[arg(long, value_name = "N")]
    baseline_rps: Option<f64>,
    /// spike: how long the baseline lasts, before and after the spike;
    /// with fuzzing: the clean phase run first at the same rate (default
    /// 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    baseline: Option<Duration>,
    /// spike: the spike as a multiple of the baseline rate (default 10)
    #[arg(long, value_name = "X")]
    spike_factor: Option<f64>,
    /// spike: how long the spike lasts (default 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    spike_duration: Option<Duration>,
    /// soak and fuzz: the rate, in iterations per second (required)
    #[arg(long, value_name = "N")]
    rps: Option<f64>,
    /// soak and fuzz: how long load runs, such as 2h or 30m (required for
    /// soak; fuzz default 1m)
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    duration: Option<Duration>,
    /// soak: the p95 slope that is a drift, as a share of the baseline p95
    /// per hour (default 5%)
    #[arg(long, value_name = "PCT", value_parser = parse_share)]
    drift_threshold: Option<f64>,
    /// Replace this share of requests with mutated variants, 0 < r <= 1
    /// (fuzz default 0.2; never against production)
    #[arg(long, value_name = "R")]
    fuzz_ratio: Option<f64>,
    /// Malformed payload families to mix in: json-syntax, deep-nesting,
    /// large-body, wide, encoding, content-type-mismatch, duplicate-keys,
    /// numbers, compression-bomb (never against production;
    /// compression-bomb needs rustman.toml [stress] families)
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    malformed: Vec<String>,
    /// An OpenAPI document: fuzzed variants break one of its schema
    /// constraints instead of mutating the saved body
    #[arg(long, value_name = "FILE")]
    spec: Option<String>,
    /// The seed of fuzzed slots and variants: the same seed and plan fuzz
    /// the same requests the same way (default: random, printed)
    #[arg(long, value_name = "N")]
    seed: Option<u64>,
    /// Wire-fault families, sent as raw HTTP/1.1 after the load:
    /// oversized-headers, many-headers, slow-body, cl-te-conflict,
    /// invalid-chunked, invalid-request-line (needs --allow-wire-faults;
    /// never against production)
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    wire: Vec<String>,
    /// Allow --wire to send raw, invalid HTTP to the target
    #[arg(long)]
    allow_wire_faults: bool,
    /// The health request file, sent once per second during and after load
    /// (default: the first GET request of the plan)
    #[arg(long, value_name = "FILE")]
    health: Option<PathBuf>,
    /// How long the target may take to recover after load (default 2m)
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    recovery_timeout: Option<Duration>,
    /// Also write the report as json, html, markdown, junit or sarif, to a
    /// file (FORMAT=PATH) or to stdout (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Stream per-second metrics to the viz companion's stress view
    /// (rustman-viz); a companion without it is a warning
    #[arg(long)]
    viz: bool,
    /// The VU pool (default: max_rps of rustman.toml [stress])
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_vus: Option<u32>,
    /// Also allow POST, PUT and PATCH
    #[arg(long)]
    allow_mutations: bool,
    /// Also allow DELETE (implies --allow-mutations; never against production)
    #[arg(long)]
    allow_destructive: bool,
    /// Confirm a production target by repeating its exact host (it must
    /// also be in rustman.toml [stress] allow_production)
    #[arg(long, value_name = "HOST")]
    production: Option<String>,
    /// Total timeout per request, replacing settings.timeout_ms
    #[arg(long, value_name = "DURATION", value_parser = parse_stress_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
    /// Count 429 (and 503 with Retry-After) as failures instead of
    /// reporting the target as rate limited
    #[arg(long)]
    treat_throttling_as_failure: bool,
    /// breakpoint: exit 1 unless max_sustainable_rps reaches N
    #[arg(long, value_name = "N")]
    require_rps: Option<f64>,
    /// Exit 1 on a finding of this severity or above: critical, high
    /// (default), medium, low or info
    #[arg(long, value_name = "SEVERITY", default_value = "high")]
    fail_on: Severity,
}

/// `5%` or `0.05`.
fn parse_share(text: &str) -> Result<f64, String> {
    let text = text.trim();
    let value = match text.strip_suffix('%') {
        Some(pct) => pct.trim().parse::<f64>().map(|p| p / 100.0),
        None => text.parse::<f64>(),
    };
    value
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| format!("'{text}' is not a share, such as 5% or 0.05"))
}

impl RunArgs {
    fn mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::Breakpoint)
    }

    fn mode_spec(&self) -> Result<ModeSpec, ConfigError> {
        Ok(match self.mode() {
            Mode::Breakpoint => {
                let d = BreakpointSpec::default();
                ModeSpec::Breakpoint(BreakpointSpec {
                    start_rps: self.start_rps.unwrap_or(d.start_rps),
                    step_factor: self.step_factor.unwrap_or(d.step_factor),
                    step_duration: self.step_duration.unwrap_or(d.step_duration),
                    refine: self.refine.unwrap_or(d.refine),
                })
            }
            Mode::Spike => {
                let d = SpikeSpec::default();
                ModeSpec::Spike(SpikeSpec {
                    baseline_rps: self.baseline_rps.unwrap_or(d.baseline_rps),
                    baseline: self.baseline.unwrap_or(d.baseline),
                    spike_factor: self.spike_factor.unwrap_or(d.spike_factor),
                    spike_duration: self.spike_duration.unwrap_or(d.spike_duration),
                })
            }
            Mode::Soak => ModeSpec::Soak(SoakSpec {
                rps: self.required_rps()?,
                duration: self
                    .duration
                    .ok_or_else(|| usage("stress: soak needs --duration".into()))?,
                drift_threshold: self
                    .drift_threshold
                    .unwrap_or(SoakSpec::default().drift_threshold),
            }),
            Mode::Fuzz => ModeSpec::Fuzz(FlatSpec {
                rps: self.required_rps()?,
                duration: self.duration.unwrap_or(Duration::from_secs(60)),
            }),
        })
    }

    fn required_rps(&self) -> Result<f64, ConfigError> {
        self.rps.ok_or_else(|| {
            usage(format!(
                "stress: {} needs --rps",
                ModeKind::from(self.mode())
            ))
        })
    }

    /// Fuzz mode mixes in mutated requests by default.
    fn fuzz_ratio(&self) -> Option<f64> {
        match (self.fuzz_ratio, self.mode()) {
            (None, Mode::Fuzz) => Some(0.2),
            (ratio, _) => ratio,
        }
    }
}

pub fn run(args: &StressArgs) -> Result<u8, CliError> {
    match &args.command {
        Some(StressCommand::Export(export)) => export_command(export).map(|()| SUCCESS),
        None => run_stress(&args.run),
    }
}

fn run_stress(args: &RunArgs) -> Result<u8, CliError> {
    let Some(target) = args.target.clone() else {
        return Err(usage("stress: --target is required".into()).into());
    };
    rustman_report::render::one_stdout(&args.report).map_err(ConfigError::Usage)?;
    if args.require_rps.is_some() && args.mode() != Mode::Breakpoint {
        return Err(usage("stress: --require-rps applies to breakpoint runs".into()).into());
    }
    let mode = args.mode_spec()?;
    let ws = args.selection.workspace()?;
    let environment = args.selection.environment(&ws)?.map(|e| e.name.clone());
    let cwd = current_dir()?;
    let health = match &args.health {
        Some(path) => scopes(&ws, &cwd, std::slice::from_ref(path))?
            .into_iter()
            .next(),
        None => None,
    };
    let scopes = scopes(&ws, &cwd, &args.paths)?;
    let store = Arc::new(secret_store(&ws.root)?);
    let runtime = runtime()?;
    let spec = match &args.spec {
        Some(source) => Some(runtime.block_on(spec_model(source))?),
        None => None,
    };
    let (interrupt, cancel) = (CancellationToken::new(), CancellationToken::new());
    let input = StressInput {
        workspace: Arc::new(ws),
        environment: environment.clone(),
        secrets: store.clone(),
        scopes,
        mode,
        target: target.clone(),
        permission: Permission::from_flags(args.allow_mutations, args.allow_destructive),
        production: args.production.clone(),
        fuzz_ratio: args.fuzz_ratio(),
        malformed: args.malformed.clone(),
        wire: args.wire.clone(),
        allow_wire_faults: args.allow_wire_faults,
        seed: args.seed,
        spec,
        fuzz_baseline: args.baseline,
        slos: args.slos.clone(),
        overrides: overrides(&args.vars)?,
        timeout: args.timeout,
        insecure: args.insecure,
        treat_throttling_as_failure: args.treat_throttling_as_failure,
        max_vus: args.max_vus,
        health,
        recovery_timeout: args.recovery_timeout,
        interrupt: interrupt.clone(),
        cancel: cancel.clone(),
        tuning: Tuning::default(),
    };
    let workspace = input.workspace.clone();
    let mode = input.mode.clone();
    let stress = Stress::prepare(input)?;
    print_warnings(stress.warnings());
    eprintln!("stress: seed {}", stress.seed());
    let shown = show_live(args, &target, &stress);
    let notice = "interrupted: stopping the load (Ctrl+C again to cancel requests in flight)";
    let outcome = runtime.block_on(async {
        stop_then_cancel_on_ctrl_c(&interrupt, &cancel, Some(notice.to_owned()));
        stress.run().await
    });
    shown.finish();
    let outcome = outcome?;
    print_warnings(&store.take_warnings());
    print_warnings(&outcome.warnings);
    print_summary(&render(&outcome), &args.report);
    let id = export::new_id(&outcome.started_at);
    let context = StressReportContext {
        id: &id,
        target: &target,
        environment: environment.as_deref(),
        mode: &mode,
        fuzz_ratio: args.fuzz_ratio(),
        malformed: &args.malformed,
        wire: &args.wire,
        workspace: &workspace,
    };
    let report = stress_report(&outcome, &context);
    export::save(&workspace.root, &report).map_err(export_error)?;
    eprintln!(
        "stress: report saved as {id} (export findings with `rustman stress export {id} --findings all --into <dir>`)"
    );
    super::report::write_all(&args.report, &Report::Stress(Box::new(report)))?;
    Ok(exit_code(&outcome, args.require_rps, args.fail_on))
}

/// The dashboard (stdout is a terminal) or a line per step, and the viz
/// stream with `--viz`. Viz problems are a warning, shown once.
fn show_live(args: &RunArgs, target: &str, stress: &Stress) -> StressLiveHandle {
    let title = format!(
        "rustman stress {} {target}",
        args.mode().to_possible_value_name()
    );
    let viz = args
        .viz
        .then(|| stress_live::start_viz(&Discovery::from_env(), &title));
    let viz = viz.and_then(|started| started.map_err(|w| print_warnings(&[w])).ok());
    let on_stdout = report_on_stdout(&args.report);
    let lines: Box<dyn std::io::Write + Send> = if on_stdout {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    };
    let options = StressLiveOptions {
        dashboard: StressLiveOptions::dashboard(on_stdout),
        title,
        lines,
        viz,
    };
    stress_live::spawn(stress.live(), stress.metrics(), options)
}

/// Whether a report is written to stdout.
fn report_on_stdout(targets: &[ReportTarget]) -> bool {
    targets.iter().any(|t| t.path.is_none())
}

/// The summary on stdout, or on stderr when a report goes to stdout.
fn print_summary(text: &str, targets: &[ReportTarget]) {
    if report_on_stdout(targets) {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

/// `rustman stress export <id> --findings <ids|all> --into <dir>`.
fn export_command(args: &ExportArgs) -> Result<(), CliError> {
    let ws = args.selection.workspace()?;
    let report = export::load(&ws.root, &args.id).map_err(export_error)?;
    let selected = export::select(&report, &args.findings).map_err(export_error)?;
    let written = export::export(&selected, &args.into).map_err(export_error)?;
    println!("wrote {} files into {}", written.len(), args.into.display());
    for file in &written {
        println!("  {}", file.display());
    }
    Ok(())
}

fn export_error(err: ExportError) -> CliError {
    usage(err.to_string()).into()
}

impl Mode {
    fn to_possible_value_name(self) -> &'static str {
        ModeKind::from(self).name()
    }
}

/// The model of the OpenAPI document `source` (a file or URL), for
/// schema-aware fuzzing.
async fn spec_model(source: &str) -> Result<rustman_explore::ApiModel, ConfigError> {
    let options = rustman_explore::ImportOptions::default();
    rustman_explore::import(source, &options)
        .await
        .map(|imported| imported.model)
        .map_err(|e| usage(format!("stress: --spec {source}: {e}")))
}

/// 130 when interrupted; 1 when a finding reaches `fail_on` or the
/// breakpoint fell short of `require_rps`; 0 otherwise. A break alone is
/// not a failure.
fn exit_code(outcome: &StressOutcome, require_rps: Option<f64>, fail_on: Severity) -> u8 {
    if outcome.interrupted {
        return INTERRUPTED;
    }
    let finding = outcome.findings.iter().any(|f| f.severity <= fail_on);
    let short = require_rps.is_some_and(|required| {
        let reached = outcome
            .breakpoint
            .as_ref()
            .and_then(|bp| bp.max_sustainable_rps);
        reached.is_none_or(|rps| rps < required)
    });
    if finding || short { ASSERTION } else { SUCCESS }
}

fn usage(message: String) -> ConfigError {
    ConfigError::Usage(message)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rustman_bench::stress::{BreakpointResult, StopReason, StressFinding};
    use rustman_bench::{LatencyKind, Metrics};

    use super::*;

    fn outcome(max_sustainable: Option<f64>, findings: Vec<StressFinding>) -> StressOutcome {
        StressOutcome {
            mode: ModeKind::Breakpoint,
            seed: 42,
            fuzz: None,
            wire: Vec::new(),
            slos: Vec::new(),
            summary: Metrics::new(LatencyKind::Corrected, Vec::new()).summary(),
            series: Vec::new(),
            windows: Vec::new(),
            breakpoint: Some(BreakpointResult {
                steps: Vec::new(),
                max_sustainable_rps: max_sustainable,
                breaking_rps: Some(300.0),
                failure_mode: None,
                rate_limited_at: None,
                stop: StopReason::Refined,
                max_rps: 500.0,
            }),
            spike: None,
            soak: None,
            findings,
            killed: None,
            recovery: rustman_bench::stress::recovery::RecoveryResult::skipped(None, "test"),
            client_saturated: false,
            interrupted: false,
            reached_max_duration: false,
            warnings: Vec::new(),
            load_started: Instant::now(),
            started_at: String::new(),
            finished_at: String::new(),
        }
    }

    #[test]
    fn a_break_alone_passes_and_a_capacity_requirement_can_fail() {
        let found = outcome(Some(84.0), Vec::new());
        assert_eq!(exit_code(&found, None, Severity::High), SUCCESS);
        assert_eq!(exit_code(&found, Some(100.0), Severity::High), ASSERTION);
        assert_eq!(exit_code(&found, Some(80.0), Severity::High), SUCCESS);
        let nothing_held = outcome(None, Vec::new());
        assert_eq!(
            exit_code(&nothing_held, Some(1.0), Severity::High),
            ASSERTION
        );
    }

    #[test]
    fn findings_fail_from_the_fail_on_severity() {
        let drift = StressFinding::new("latency-drift", Severity::Medium, String::new());
        let run = outcome(Some(84.0), vec![drift]);
        assert_eq!(exit_code(&run, None, Severity::High), SUCCESS);
        assert_eq!(exit_code(&run, None, Severity::Medium), ASSERTION);
        let mut interrupted = run;
        interrupted.interrupted = true;
        assert_eq!(exit_code(&interrupted, None, Severity::Medium), INTERRUPTED);
    }

    #[test]
    fn shares_are_percentages_or_fractions() {
        assert_eq!(parse_share("5%"), Ok(0.05));
        assert_eq!(parse_share("0.1"), Ok(0.1));
        assert!(parse_share("-1%").is_err());
        assert!(parse_share("lots").is_err());
    }
}
