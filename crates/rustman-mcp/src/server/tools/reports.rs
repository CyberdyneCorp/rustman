//! Report tools (spec "OpenAPI and exploration tools", `test-reports`):
//! `get_report` and `list_findings`, over exploration reports on disk, the
//! run reports of this session's jobs, and report files of the workspace
//! (such as a `rustman bench --report json=...` report: agents read bench
//! reports, they never start a bench).

use std::io::Read;

use rustman_explore::explore::session::Session;
use rustman_report::bench::BenchReport;
use rustman_report::exploration::{ExplorationReport, ReportFinding, Severity};
use rustman_report::run::Status;
use rustman_report::{RenderFormat, RenderOptions, Report, RunReport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::explore::{REPORT_URI_PREFIX, session_error};
use super::{Call, output};
use crate::server::context::{AgentContext, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::JobState;
use crate::server::limits::{paginate, snapshot};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetReportArgs {
    /// An exploration id (`exp-...`), the job id of a run or exploration,
    /// or a JSON report file relative to the workspace root, such as
    /// `out/bench.json` (default: the latest exploration).
    pub id: Option<String>,
    /// `summary` (default), or the whole report as `json`, `markdown`,
    /// `junit`, `sarif` or `html`.
    pub format: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReportResult {
    /// The report's id: an exploration id, or a run's job id.
    pub id: String,
    /// `run`, `exploration`, `contract` or `bench`.
    pub kind: String,
    pub format: String,
    /// The whole report, also readable as a resource.
    pub uri: String,
    /// Totals and findings (format `summary`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    /// The rendered report (every other format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListFindingsArgs {
    /// The exploration id, or its job id (default: the latest exploration).
    pub exploration_id: Option<String>,
    /// Only findings at or above this severity: critical, high, medium, low
    /// or info.
    pub severity: Option<String>,
    /// Only findings of operations containing this text, such as
    /// `GET /users/{id}` or `/users` (any case).
    pub operation: Option<String>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Items per page (default 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindingList {
    pub exploration_id: String,
    pub items: Vec<ReportFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many findings match, over every page.
    pub total: usize,
}

/// The report `id` names: a run job's, or an exploration's (by job id,
/// exploration id, or the latest).
pub fn find_report(ctx: &AgentContext, id: Option<&str>) -> Result<(String, Report), ToolError> {
    if let Some(file) = id.filter(|id| id.ends_with(".json")) {
        return Ok((file.to_owned(), report_file(ctx, file)?));
    }
    if let Some(job) = id.and_then(|id| ctx.jobs().get(id))
        && job.exploration.is_none()
    {
        if job.state() == JobState::Running {
            return Err(ToolError::new(
                ErrorCode::InvalidInput,
                format!(
                    "{} is still running ({}); check again later",
                    job.id,
                    job.counted()
                ),
            ));
        }
        let report = job.report().ok_or_else(|| {
            let reason = job.error().unwrap_or_else(|| "no report".into());
            ToolError::new(
                ErrorCode::NotFound,
                format!("{} has no report: {reason}", job.id),
            )
        })?;
        let report = serde_json::from_value::<RunReport>(report)
            .map_err(|e| ToolError::internal(e.to_string()))?;
        return Ok((job.id.clone(), Report::Run(Box::new(report))));
    }
    let exploration = exploration_id(ctx, id)?;
    let report = exploration_report(ctx, &exploration)?;
    Ok((exploration, Report::Exploration(Box::new(report))))
}

/// The largest report file `get_report` reads.
const MAX_REPORT_FILE: u64 = 32 * 1024 * 1024;

/// The report in `file`, relative to the workspace root. A symlink leading
/// out of the workspace is refused, and so is a file over
/// [`MAX_REPORT_FILE`]. A file that is not a report is refused without the
/// parser's message, which could quote the file.
fn report_file(ctx: &AgentContext, file: &str) -> Result<Report, ToolError> {
    let root = &ctx.workspace().root;
    let not_found = || ToolError::new(ErrorCode::NotFound, format!("no report file {file}"));
    let path = root.join(super::execute::contained(file)?);
    let real = std::fs::canonicalize(&path).map_err(|_| not_found())?;
    let real_root = std::fs::canonicalize(root).map_err(|_| not_found())?;
    if !real.starts_with(&real_root) {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            format!("'{file}' is not a path inside the workspace"),
        ));
    }
    let opened = std::fs::File::open(&real).map_err(|_| not_found())?;
    let mut text = String::new();
    opened
        .take(MAX_REPORT_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|_| not_found())?;
    if text.len() as u64 > MAX_REPORT_FILE {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            format!("{file} is over 32 MiB, more than a report file may be"),
        ));
    }
    Report::parse(&text).map_err(|_| {
        ToolError::new(
            ErrorCode::InvalidInput,
            format!("{file} is not a rustman report (a --report json=... file)"),
        )
    })
}

/// The exploration `id` names: itself, the one its job drives, or the
/// latest.
fn exploration_id(ctx: &AgentContext, id: Option<&str>) -> Result<String, ToolError> {
    let root = &ctx.workspace().root;
    match id {
        Some(id) => match ctx.jobs().get(id) {
            Some(job) => job.exploration.clone().ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidInput,
                    format!("{id} is not an exploration"),
                )
            }),
            None => Ok(id.to_owned()),
        },
        None => Ok(Session::latest(root).map_err(session_error)?.state.id),
    }
}

/// The finished report of exploration `id`.
pub fn exploration_report(ctx: &AgentContext, id: &str) -> Result<ExplorationReport, ToolError> {
    let session = Session::open(&ctx.workspace().root, id).map_err(session_error)?;
    let path = session.report_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        ToolError::new(
            ErrorCode::NotFound,
            format!(
                "exploration {id} has no report yet (it is {}); check explore_status",
                session.state.state
            ),
        )
    })?;
    match Report::parse(&text) {
        Ok(Report::Exploration(report)) => Ok(*report),
        Ok(Report::Run(_) | Report::Contract(_) | Report::Bench(_)) => Err(ToolError::internal(
            format!("{} is not an exploration report", path.display()),
        )),
        Err(err) => Err(ToolError::internal(err.to_string())),
    }
}

fn exploration_summary(report: &ExplorationReport) -> Value {
    let findings: Vec<Value> = report
        .findings
        .iter()
        .map(|f| {
            json!({
                "id": f.id,
                "rule": f.rule,
                "severity": f.severity,
                "operation": f.operation,
                "occurrences": f.occurrences,
            })
        })
        .collect();
    json!({
        "target": report.target,
        "environment": report.environment,
        "seed": report.seed,
        "summary": report.summary,
        "coverage": {
            "operations": report.coverage.operations,
            "statuses": report.coverage.statuses,
        },
        "findings": findings,
        "left_behind": report.left_behind.len(),
    })
}

fn bench_summary(report: &BenchReport) -> Value {
    json!({
        "target": report.target,
        "environment": report.environment,
        "profile": report.profile,
        "aborted": report.aborted,
        "passed": report.passed(),
        "totals": report.totals,
        "overall": report.overall,
        "thresholds": report.thresholds,
    })
}

fn run_summary(report: &RunReport) -> Value {
    let problems: Vec<Value> = report
        .requests
        .iter()
        .filter(|r| r.status != Status::Passed)
        .map(|r| json!({"id": r.id, "status": r.status, "error": r.error}))
        .collect();
    json!({
        "environment": report.environment,
        "started_at": report.started_at,
        "summary": report.summary,
        "not_passed": problems,
    })
}

pub fn get_report(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GetReportArgs = call.args()?;
    let (id, report) = find_report(&call.ctx, args.id.as_deref())?;
    let format = args.format.unwrap_or_else(|| "summary".into());
    let (summary, text) = if format == "summary" {
        let summary = match &report {
            Report::Run(r) => run_summary(r),
            Report::Exploration(r) => exploration_summary(r),
            Report::Contract(r) => json!({ "mode": r.mode, "summary": r.summary }),
            Report::Bench(r) => bench_summary(r),
        };
        (Some(summary), None)
    } else {
        let parsed: RenderFormat = format
            .parse()
            .map_err(|e: String| ToolError::new(ErrorCode::InvalidInput, e))?;
        let text = rustman_report::render(&report, parsed, &RenderOptions::default());
        (None, Some(text))
    };
    let result = ReportResult {
        uri: format!("{REPORT_URI_PREFIX}{id}"),
        kind: report.kind().to_owned(),
        id,
        format,
        summary,
        text,
    };
    let line = format!("{} report {} ({})", result.kind, result.id, result.format);
    output(&result, line)
}

/// Whether `finding` passes the filters: `at_least` and `operation`.
fn keeps(finding: &ReportFinding, at_least: Option<Severity>, operation: Option<&str>) -> bool {
    let rank = |s: Severity| Severity::ALL.iter().position(|x| *x == s);
    let severe = at_least.is_none_or(|min| rank(finding.severity) <= rank(min));
    let of_operation = operation.is_none_or(|op| {
        finding
            .operation
            .to_lowercase()
            .contains(&op.to_lowercase())
    });
    severe && of_operation
}

pub fn list_findings(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ListFindingsArgs = call.args()?;
    let at_least = match &args.severity {
        Some(text) => Some(
            text.parse::<Severity>()
                .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e))?,
        ),
        None => None,
    };
    let id = exploration_id(&call.ctx, args.exploration_id.as_deref())?;
    let report = exploration_report(&call.ctx, &id)?;
    let found: Vec<ReportFinding> = report
        .findings
        .into_iter()
        .filter(|f| keeps(f, at_least, args.operation.as_deref()))
        .collect();
    let total = found.len();
    let keys = found.iter().map(|f| f.id.clone()).collect::<Vec<_>>();
    let page = paginate(found, snapshot(&keys), args.cursor.as_deref(), args.limit)?;
    let shown = page.items.len();
    let list = FindingList {
        exploration_id: id,
        items: page.items,
        next_cursor: page.next_cursor,
        total,
    };
    output(&list, format!("{shown} of {total} findings"))
}
