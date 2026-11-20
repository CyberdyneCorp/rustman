//! The run report (`rustman.report/v1` with `kind: run`) as owned data that
//! both serializes and deserializes, so `report render` and `report diff`
//! read what `rustman run --reporter json` wrote. It is built from a
//! [`RunRecord`], whose every text field is already redacted.

use rustman_core::assertions::{AssertionResult, Outcome};
use rustman_core::captures::{CaptureOutcome, CaptureResult};
use rustman_core::run::{
    DataRowRecord, ExchangeRecord, HeaderRecord, JsonrpcRecord, RealtimeRecord, RequestRecord,
    RequestStatus, RunRecord, SentRequest, SkipReason, Summary,
};
use rustman_core::snapshot::{SnapshotCounts, SnapshotResult};
use rustman_core::trace::{ServerTiming, TraceRecord};
use serde::{Deserialize, Serialize};

use crate::SCHEMA;

/// The `kind` of a `rustman run` (and `rustman send`) report.
pub const KIND_RUN: &str = "run";

/// A whole run report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub schema: String,
    pub kind: String,
    pub rustman_version: String,
    pub started_at: String,
    pub environment: Option<String>,
    pub summary: RunSummary,
    pub requests: Vec<RunRequest>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errored: usize,
    pub skipped: usize,
    pub duration_ms: f64,
    pub interrupted: bool,
    /// How the run's snapshot assertions ended (additive); absent without any.
    #[serde(default, skip_serializing_if = "SnapshotCounts::is_empty")]
    pub snapshots: SnapshotCounts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Errored,
    Skipped,
}

/// One request of the run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRequest {
    pub id: String,
    pub name: String,
    pub file: String,
    pub iteration: u32,
    /// How many iterations the run had; not part of `rustman.report/v1`,
    /// so 0 in a report read back from JSON.
    #[serde(skip)]
    pub iterations: u32,
    /// The data row of a `--data` run; absent from every other run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_row: Option<DataRow>,
    pub status: Status,
    /// `bail`, `interrupted` or `unsupported_kind`.
    pub skip_reason: Option<String>,
    pub error: Option<String>,
    pub exchange: Option<Exchange>,
    /// The JSON-RPC envelopes, attempts and subscription events of a
    /// `kind: jsonrpc` request; absent from every other request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<JsonrpcRecord>,
    /// The gRPC exchange (`protocol: grpc`) of a `kind: grpc` request;
    /// absent from every other request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<serde_json::Value>,
    /// The handshake, timeline and end of a `kind: ws` or `kind: sse`
    /// session; absent from every other request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime: Option<RealtimeRecord>,
    pub assertions: Vec<Assertion>,
    pub captures: Vec<Capture>,
    pub warnings: Vec<String>,
    pub duration_ms: f64,
    /// The trace context the request carried (additive); absent without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    pub request: SentRequest,
    pub response: Response,
    pub timings: Timings,
    pub redirects: Vec<Redirect>,
    pub insecure: bool,
    /// The response's `Server-Timing` metrics (additive); absent without any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_timings: Vec<ServerTiming>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub http_version: String,
    pub headers: Vec<HeaderRecord>,
    pub body: Option<String>,
    pub body_bytes: u64,
    pub body_truncated: bool,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    pub dns_ms: Option<f64>,
    pub connect_ms: Option<f64>,
    pub tls_ms: Option<f64>,
    pub ttfb_ms: f64,
    pub download_ms: f64,
    pub total_ms: f64,
    /// The challenge round trip before the final one (Digest's `401`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_ms: Option<f64>,
}

/// The data row a request of a data-driven (`--data`) run used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRow {
    /// 1-based: the row number, without the header.
    pub index: u32,
    /// How many rows the run iterates over.
    pub total: u32,
    /// The `--data-label` column's value, else `row <index>`.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redirect {
    pub status: u16,
    pub url: String,
}

/// One assertion result: `pass`, `fail` or `error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    pub kind: String,
    pub outcome: String,
    pub label: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub message: String,
    /// A `snapshot` assertion's state, file and structured diff (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotResult>,
}

impl Assertion {
    pub fn passed(&self) -> bool {
        self.outcome == "pass"
    }
}

/// One capture result; it never holds the captured value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub var: String,
    pub outcome: String,
    pub message: Option<String>,
    pub required: bool,
    pub persisted: bool,
    pub secret: bool,
}

impl RunReport {
    /// The report of `run`.
    pub fn new(run: &RunRecord) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            kind: KIND_RUN.to_owned(),
            rustman_version: rustman_core::VERSION.to_owned(),
            started_at: run.started_at.clone(),
            environment: run.environment.clone(),
            summary: summary(&run.summary),
            requests: run.requests.iter().map(request).collect(),
        }
    }
}

impl RunReport {
    /// Whether the run repeated its requests (`--iterations` or `--data`),
    /// so reporters label each request with its iteration.
    pub fn is_iterated(&self) -> bool {
        self.requests
            .iter()
            .any(|r| r.iterations > 1 || r.iteration > 1 || r.data_row.is_some())
    }
}

impl RunRequest {
    /// The label of this request's iteration in an iterated run: the data
    /// row's, else the iteration index. `None` when `iterated` is false.
    pub fn iteration_label(&self, iterated: bool) -> Option<String> {
        match &self.data_row {
            Some(row) => Some(row.label.clone()),
            None => iterated.then(|| self.iteration.to_string()),
        }
    }

    /// Why it did not pass: its error, failed assertions and warnings.
    pub fn reasons(&self) -> Vec<String> {
        let mut reasons: Vec<String> = self.error.iter().cloned().collect();
        reasons.extend(
            self.assertions
                .iter()
                .filter(|a| !a.passed())
                .map(Assertion::reason),
        );
        reasons.extend(self.warnings.iter().map(|w| format!("warning: {w}")));
        reasons
    }
}

impl RunRequest {
    /// The snapshots this request recorded or updated, one line each.
    pub fn snapshot_notes(&self) -> Vec<String> {
        self.assertions
            .iter()
            .filter_map(|a| a.snapshot.as_ref())
            .filter(|s| s.wrote())
            .map(SnapshotResult::note)
            .collect()
    }
}

impl Assertion {
    /// As a failure lists it (the `Display` of [`AssertionResult`]).
    fn reason(&self) -> String {
        match self.outcome.as_str() {
            "fail" => self.message.clone(),
            "error" => format!("error: {}", self.message),
            _ => format!("pass: {}", self.label),
        }
    }
}

fn summary(s: &Summary) -> RunSummary {
    RunSummary {
        total: s.total,
        passed: s.passed,
        failed: s.failed,
        errored: s.errored,
        skipped: s.skipped,
        duration_ms: s.duration_ms,
        interrupted: s.interrupted,
        snapshots: s.snapshots,
    }
}

fn request(r: &RequestRecord) -> RunRequest {
    RunRequest {
        id: r.id.clone(),
        name: r.name.clone(),
        file: r.file.clone(),
        iteration: r.iteration,
        iterations: r.iterations,
        data_row: r.data_row.as_ref().map(data_row),
        status: match r.status {
            RequestStatus::Passed => Status::Passed,
            RequestStatus::Failed => Status::Failed,
            RequestStatus::Errored => Status::Errored,
            RequestStatus::Skipped => Status::Skipped,
        },
        skip_reason: r.skip_reason.map(skip_reason),
        error: r.error.clone(),
        exchange: r.exchange.as_ref().map(exchange),
        jsonrpc: r.jsonrpc.clone(),
        grpc: r.grpc.clone(),
        realtime: r.realtime.clone(),
        assertions: r.assertions.iter().map(assertion).collect(),
        captures: r.captures.iter().map(capture).collect(),
        warnings: r.warnings.clone(),
        duration_ms: r.duration_ms,
        trace: r.trace.clone(),
    }
}

fn data_row(row: &DataRowRecord) -> DataRow {
    DataRow {
        index: row.index,
        total: row.total,
        label: row.label.clone(),
    }
}

fn skip_reason(reason: SkipReason) -> String {
    match reason {
        SkipReason::Bail => "bail",
        SkipReason::Interrupted => "interrupted",
        SkipReason::UnsupportedKind => "unsupported_kind",
    }
    .to_owned()
}

fn exchange(e: &ExchangeRecord) -> Exchange {
    let t = &e.timings;
    Exchange {
        request: e.request.clone(),
        response: Response {
            status: e.response.status,
            http_version: e.response.http_version.clone(),
            headers: e.response.headers.clone(),
            body: e.response.body.clone(),
            body_bytes: e.response.body_bytes,
            body_truncated: e.response.body_truncated,
            binary: e.response.binary,
        },
        timings: Timings {
            dns_ms: t.dns_ms,
            connect_ms: t.connect_ms,
            tls_ms: t.tls_ms,
            ttfb_ms: t.ttfb_ms,
            download_ms: t.download_ms,
            total_ms: t.total_ms,
            auth_ms: t.auth_ms,
        },
        redirects: e
            .redirects
            .iter()
            .map(|r| Redirect {
                status: r.status,
                url: r.url.clone(),
            })
            .collect(),
        insecure: e.insecure,
        server_timings: e.server_timings.clone(),
    }
}

fn assertion(a: &AssertionResult) -> Assertion {
    Assertion {
        kind: a.kind.to_owned(),
        outcome: match a.outcome {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Error => "error",
        }
        .to_owned(),
        label: a.label.clone(),
        expected: a.expected.clone(),
        actual: a.actual.clone(),
        message: a.message.clone(),
        snapshot: a.snapshot.clone(),
    }
}

fn capture(c: &CaptureResult) -> Capture {
    Capture {
        var: c.var.clone(),
        outcome: match c.outcome {
            CaptureOutcome::Captured => "captured",
            CaptureOutcome::NoMatch => "no_match",
            CaptureOutcome::Error => "error",
        }
        .to_owned(),
        message: c.message.clone(),
        required: c.required,
        persisted: c.persisted,
        secret: c.secret,
    }
}
