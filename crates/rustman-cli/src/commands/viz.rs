//! The viz companion from the CLI (openspec change `add-raven-visualizer`,
//! tasks 2.3–2.6): `rustman viz <REPORT>` opens a saved report, and
//! `rustman run --viz` / `rustman explore run --viz` stream their progress
//! to a live dashboard.
//!
//! The companion is optional: when it cannot be used, `--viz` prints why
//! and the command carries on without visuals, with its exit code
//! unchanged; a companion that exits or stops reading mid-run is detached
//! from with a warning (`rustman-viz-client`). Everything sent is redacted:
//! run records with every secret the run has seen, exploration outcomes
//! as they were journaled.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, ValueEnum};
use rustman_core::assertions::Outcome as AssertionOutcome;
use rustman_core::run::{RequestRecord, RequestStatus, RunRecord, SkipReason, now_rfc3339};
use rustman_core::{ConfigError, Redactor};
use rustman_explore::explore::exec::{Outcome as ProbeOutcome, Status};
use rustman_explore::explore::plan::Plan;
use rustman_explore::explore::report::ExplorationReport;
use rustman_report::Report;
use rustman_report::exploration::Severity as ReportSeverity;
use rustman_viz_client::{ConnectOptions, Discovery, Stream, StreamOptions, VizError, Warn};
use rustman_viz_protocol::{
    Assertions, ExchangeCompleted, ExplorationFinished, ExplorationStarted, Finding, LoadReport,
    Message, OpenView, Outcome, ProbeCompleted, ReportKind, RunFinished, RunStarted, RunState,
    RunSummary, Severity, View,
};

use crate::exit::{self, CliError};

#[derive(Debug, Args)]
pub struct VizArgs {
    /// A JSON report from `rustman run --reporter json` or `rustman explore
    /// run`
    #[arg(value_name = "REPORT")]
    report: PathBuf,
    /// The view to open: dashboard (default), or graph for the API graph of
    /// an exploration
    #[arg(long, value_name = "VIEW", value_enum)]
    view: Option<ViewArg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ViewArg {
    Dashboard,
    Graph,
}

/// `rustman viz <REPORT>`: open a saved report in the matching view. A
/// file that is not a Rustman report exits 2; so does a companion that
/// cannot be used, after saying why and how to get an HTML report.
pub fn run(args: &VizArgs) -> Result<u8, CliError> {
    let shown = args.report.display().to_string();
    let not_a_report = || ConfigError::Usage(format!("{shown} is not a Rustman report"));
    let text = std::fs::read_to_string(&args.report).map_err(|_| not_a_report())?;
    let report = Report::parse(&text).map_err(|_| not_a_report())?;
    let kind = match report {
        Report::Run(_) => ReportKind::Run,
        Report::Exploration(_) => ReportKind::Exploration,
    };
    let view = match (args.view, kind) {
        (Some(ViewArg::Graph), ReportKind::Exploration) => View::ApiGraph,
        (Some(ViewArg::Graph), _) => {
            return Err(ConfigError::Usage(format!(
                "--view graph needs an exploration report; {shown} is a run report"
            ))
            .into());
        }
        (_, ReportKind::Run) => View::RunDashboard,
        _ => View::ExplorationDashboard,
    };
    let path = dunce::canonicalize(&args.report).unwrap_or_else(|_| args.report.clone());
    let mut stream = match open(view, &shown, None) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "hint: render it as HTML instead: rustman report render {shown} --format html -o report.html"
            );
            return Ok(exit::CONFIG);
        }
    };
    stream.send(&Message::LoadReport(LoadReport {
        path: path.display().to_string(),
        kind,
        view: Some(view),
    }));
    stream.close();
    eprintln!("Opened {shown} in {}", stream.viewer());
    Ok(exit::SUCCESS)
}

/// Find, probe and start the companion, and open `view`.
fn open(view: View, title: &str, root: Option<&Path>) -> Result<Stream, VizError> {
    let program = Discovery::from_env().locate()?;
    let options = ConnectOptions {
        log: root.and_then(log_file),
        ..ConnectOptions::new(format!("rustman {}", rustman_core::VERSION))
    };
    let connection = rustman_viz_client::connect(&program, &options)?;
    let warn: Warn = Arc::new(|warning: &str| eprintln!("warning: {warning}"));
    let stream = Stream::start(connection, StreamOptions::default(), warn);
    stream.send(&Message::OpenView(OpenView {
        view,
        title: Some(title.to_owned()),
    }));
    Ok(stream)
}

/// The companion's stderr log in the workspace's state directory, when
/// there is one.
fn log_file(root: &Path) -> Option<PathBuf> {
    let state = root.join(".rustman");
    state.is_dir().then(|| state.join("viz.log"))
}

/// A live view for `--viz`, or `None` after printing why there is none.
fn live(view: View, title: &str, root: Option<&Path>) -> Option<Stream> {
    match open(view, title, root) {
        Ok(stream) => Some(stream),
        Err(e) => {
            eprintln!("warning: {}", unavailable(&e));
            eprintln!("hint: add --report html=report.html to get an HTML report instead");
            None
        }
    }
}

/// Why `--viz` continues without visuals.
fn unavailable(error: &VizError) -> String {
    match error {
        VizError::NotFound(detail) => {
            format!("Viz companion not found; continuing without visuals ({detail})")
        }
        other => format!("{other}; continuing without visuals"),
    }
}

fn millis(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 {
        ms.round() as u64
    } else {
        0
    }
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// `rustman run --viz`: the run dashboard, fed as requests finish.
pub struct RunViz {
    stream: Stream,
}

impl RunViz {
    /// Open the run dashboard for `total` planned requests; `None` when
    /// the companion cannot be used.
    pub fn start(
        title: &str,
        root: &Path,
        environment: Option<String>,
        total: usize,
    ) -> Option<Self> {
        let stream = live(View::RunDashboard, title, Some(root))?;
        stream.send(&Message::RunStarted(RunStarted {
            started_at: now_rfc3339(),
            environment,
            total: Some(count(total)),
        }));
        Some(Self { stream })
    }

    /// One finished request; it never blocks.
    pub fn exchange(&self, record: &RequestRecord, redactor: &Redactor) {
        let message = Message::ExchangeCompleted(Box::new(exchange_message(record, redactor)));
        self.stream.send(&message);
    }

    /// The run is over: its summary, then `close`.
    pub fn finish(mut self, record: &RunRecord) {
        let s = &record.summary;
        self.stream.send(&Message::RunFinished(RunFinished {
            state: if s.interrupted {
                RunState::Aborted
            } else {
                RunState::Completed
            },
            summary: RunSummary {
                total: count(s.total),
                passed: count(s.passed),
                failed: count(s.failed),
                errored: count(s.errored),
                skipped: count(s.skipped),
                duration_ms: millis(s.duration_ms),
            },
        }));
        self.stream.close();
    }
}

/// The protocol form of a finished request. The record is redacted
/// already; its URL and error are redacted again with every secret of the
/// run so far, as every other sink of a run is.
fn exchange_message(record: &RequestRecord, redactor: &Redactor) -> ExchangeCompleted {
    let (method, url, status) = record.exchange.as_ref().map_or_else(
        || (String::new(), String::new(), None),
        |e| {
            (
                e.request.method.clone(),
                redactor.text(&e.request.url).into_owned(),
                Some(e.response.status),
            )
        },
    );
    let outcome = match record.status {
        RequestStatus::Passed => Outcome::Passed,
        RequestStatus::Failed => Outcome::Failed,
        RequestStatus::Errored => Outcome::Errored,
        RequestStatus::Skipped if record.skip_reason == Some(SkipReason::Interrupted) => {
            Outcome::Cancelled
        }
        RequestStatus::Skipped => Outcome::Skipped,
    };
    let tally = |pass: bool| {
        let n = record
            .assertions
            .iter()
            .filter(|a| (a.outcome == AssertionOutcome::Pass) == pass)
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    };
    ExchangeCompleted {
        request: record.id.clone(),
        method,
        url,
        outcome,
        status,
        duration_ms: millis(record.duration_ms),
        assertions: Assertions {
            passed: tally(true),
            failed: tally(false),
        },
        error: (record.status == RequestStatus::Errored)
            .then(|| {
                record
                    .error
                    .as_deref()
                    .map(|e| redactor.text(e).into_owned())
            })
            .flatten(),
        iteration: (record.iterations > 1).then_some(record.iteration),
    }
}

/// `rustman explore run --viz`: the exploration dashboard, fed as probes
/// finish.
pub struct ExploreViz {
    stream: Stream,
    /// The operation of each probe id.
    operations: HashMap<String, String>,
    /// Findings already sent, per rule and operation (as the report counts
    /// them).
    sent: HashSet<(String, String)>,
    /// Operations that answered at least once.
    exercised: HashSet<String>,
    total_operations: usize,
}

impl ExploreViz {
    /// Open the exploration dashboard; `None` when the companion cannot
    /// be used.
    pub fn start(
        title: &str,
        root: &Path,
        plan: &Plan,
        total_operations: usize,
        base_url: &str,
    ) -> Option<Self> {
        let stream = live(View::ExplorationDashboard, title, Some(root))?;
        stream.send(&Message::ExplorationStarted(ExplorationStarted {
            started_at: now_rfc3339(),
            operations: count(total_operations),
            base_url: Some(base_url.to_owned()),
        }));
        let operations = plan
            .probes
            .iter()
            .map(|p| (p.id.clone(), p.operation.clone()))
            .collect();
        Some(Self {
            stream,
            operations,
            sent: HashSet::new(),
            exercised: HashSet::new(),
            total_operations,
        })
    }

    /// One finished probe and its new findings; it never blocks.
    pub fn probe(&mut self, outcome: &ProbeOutcome, took: Duration) {
        let operation = self
            .operations
            .get(&outcome.probe)
            .cloned()
            .unwrap_or_else(|| outcome.probe.clone());
        let status = outcome.sent.last().map(|s| s.status).filter(|s| *s != 0);
        if status.is_some() {
            self.exercised.insert(operation.clone());
        }
        self.stream.send(&Message::ProbeCompleted(ProbeCompleted {
            operation,
            outcome: probe_outcome(outcome),
            status,
            duration_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        }));
        for finding in &outcome.findings {
            let key = (finding.rule.clone(), finding.operation.clone());
            if !self.sent.insert(key) {
                continue;
            }
            self.stream.send(&Message::Finding(Finding {
                severity: severity(finding.severity),
                operation: finding.operation.clone(),
                title: finding.message.clone(),
                detail: Some(format!("rule {}, probe {}", finding.rule, finding.probe)),
            }));
        }
    }

    /// The exploration is over (`report` is `None` when it was
    /// interrupted): its coverage, then `close`.
    pub fn finish(mut self, report: Option<&ExplorationReport>) {
        let finished = match report {
            Some(r) => ExplorationFinished {
                state: RunState::Completed,
                operations_total: count(r.coverage.operations.total),
                operations_exercised: count(r.coverage.operations.covered),
                status_codes_documented: count(r.coverage.statuses.total),
                status_codes_seen: count(r.coverage.statuses.covered),
            },
            None => ExplorationFinished {
                state: RunState::Aborted,
                operations_total: count(self.total_operations),
                operations_exercised: count(self.exercised.len()),
                status_codes_documented: 0,
                status_codes_seen: 0,
            },
        };
        self.stream.send(&Message::ExplorationFinished(finished));
        self.stream.close();
    }
}

fn probe_outcome(outcome: &ProbeOutcome) -> Outcome {
    match &outcome.status {
        Status::Done if outcome.findings.is_empty() => Outcome::Passed,
        Status::Done => Outcome::Failed,
        Status::Failed(_) => Outcome::Errored,
        status if status.is_interrupted() => Outcome::Cancelled,
        Status::Blocked(_) | Status::Skipped(_) | Status::NotRun(_) => Outcome::Skipped,
    }
}

fn severity(severity: ReportSeverity) -> Severity {
    match severity {
        ReportSeverity::Critical => Severity::Critical,
        ReportSeverity::High => Severity::High,
        ReportSeverity::Medium => Severity::Medium,
        ReportSeverity::Low => Severity::Low,
        ReportSeverity::Info => Severity::Info,
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::Secret;
    use rustman_core::run::{ExchangeRecord, ResponseRecord, SentRequest, TimingsRecord};

    use super::*;

    fn record(status: RequestStatus) -> RequestRecord {
        let mut r = RequestRecord::new(
            "users/get".into(),
            "Get".into(),
            "users/get.rman.yaml".into(),
        );
        r.status = status;
        r.iterations = 1;
        r.iteration = 1;
        r.duration_ms = 12.4;
        r
    }

    #[test]
    fn a_request_record_becomes_an_exchange_with_secrets_redacted() {
        let secret = "s3cr3t-canary";
        let redactor = Redactor::new().with(&Secret::new("token", secret.to_owned()));
        let mut r = record(RequestStatus::Passed);
        r.exchange = Some(ExchangeRecord {
            request: SentRequest {
                method: "GET".into(),
                url: format!("https://api.test/users?t={secret}"),
                headers: Vec::new(),
                body: None,
            },
            response: ResponseRecord {
                status: 200,
                http_version: "HTTP/1.1".into(),
                headers: Vec::new(),
                body: None,
                body_bytes: 0,
                body_truncated: false,
                binary: false,
            },
            timings: TimingsRecord::default(),
            redirects: Vec::new(),
            insecure: false,
        });
        let message = exchange_message(&r, &redactor);
        assert_eq!(message.method, "GET");
        assert_eq!(message.status, Some(200));
        assert_eq!(message.outcome, Outcome::Passed);
        assert_eq!(message.duration_ms, 12);
        assert_eq!(message.iteration, None);
        let line =
            rustman_viz_protocol::encode_line(&Message::ExchangeCompleted(Box::new(message)))
                .unwrap();
        assert!(!line.contains(secret), "{line}");
        assert!(line.contains("redacted:token"), "{line}");
    }

    #[test]
    fn errors_and_skips_map_to_their_outcomes() {
        let secret = "s3cr3t-canary";
        let redactor = Redactor::new().with(&Secret::new("token", secret.to_owned()));
        let mut errored = record(RequestStatus::Errored);
        errored.error = Some(format!("dns: cannot resolve {secret}.invalid"));
        let message = exchange_message(&errored, &redactor);
        assert_eq!(message.outcome, Outcome::Errored);
        let error = message.error.unwrap();
        assert!(
            !error.contains(secret) && error.starts_with("dns:"),
            "{error}"
        );
        let mut cancelled = record(RequestStatus::Skipped);
        cancelled.skip_reason = Some(SkipReason::Interrupted);
        assert_eq!(
            exchange_message(&cancelled, &redactor).outcome,
            Outcome::Cancelled
        );
        let mut bailed = record(RequestStatus::Skipped);
        bailed.skip_reason = Some(SkipReason::Bail);
        assert_eq!(
            exchange_message(&bailed, &redactor).outcome,
            Outcome::Skipped
        );
    }

    #[test]
    fn probe_statuses_map_to_outcomes() {
        let outcome = |status| ProbeOutcome::new("p0001", status);
        assert_eq!(probe_outcome(&outcome(Status::Done)), Outcome::Passed);
        assert_eq!(
            probe_outcome(&outcome(Status::Failed("connect".into()))),
            Outcome::Errored
        );
        assert_eq!(
            probe_outcome(&outcome(Status::interrupted())),
            Outcome::Cancelled
        );
        assert_eq!(
            probe_outcome(&outcome(Status::NotRun("budget".into()))),
            Outcome::Skipped
        );
        assert_eq!(
            probe_outcome(&outcome(Status::Blocked("production".into()))),
            Outcome::Skipped
        );
    }

    #[test]
    fn unavailable_messages_name_the_reason() {
        let not_found = unavailable(&VizError::NotFound("no rustman-viz on PATH".into()));
        assert!(
            not_found.starts_with("Viz companion not found; continuing without visuals"),
            "{not_found}"
        );
        assert_eq!(
            unavailable(&VizError::Graphics),
            "Graphics unavailable (Vulkan); continuing without visuals"
        );
    }
}
