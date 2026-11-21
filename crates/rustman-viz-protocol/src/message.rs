//! The messages of `rustman-viz/1`. Every message is a JSON object whose
//! `type` field names it. Optional fields are left out when unset, and
//! fields a reader does not know are ignored.

use serde::{Deserialize, Serialize};

/// One protocol message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Producer → companion: opens every session.
    Hello(Hello),
    /// Companion → producer: the protocol is accepted.
    HelloAck(HelloAck),
    /// Companion → producer: the protocol is not supported.
    HelloReject(HelloReject),
    OpenView(OpenView),
    RunStarted(RunStarted),
    ExchangeCompleted(Box<ExchangeCompleted>),
    RunFinished(RunFinished),
    ExplorationStarted(ExplorationStarted),
    ProbeCompleted(ProbeCompleted),
    Finding(Finding),
    ExplorationFinished(ExplorationFinished),
    LoadReport(LoadReport),
    /// One second of a bench (add-load-testing): sent once a second.
    BenchTick(BenchTick),
    /// The end of a bench; never dropped.
    BenchFinished(BenchFinished),
    /// The trend of one monitor (add-scheduled-monitors), for the
    /// `monitor_trend` view.
    MonitorTrend(Box<MonitorTrend>),
    /// The latency of one method on one provider (`rustman evm probe
    /// --viz`), a cell of the latency heatmap.
    ProbeLatency(ProbeLatency),
    /// One recent block of one provider, for the block-time and gas plots.
    ChainBlock(ChainBlock),
    /// The call trace of `--call` on one provider, as a node graph.
    CallTrace(Box<CallTrace>),
    InspectResponse(Box<InspectResponse>),
    /// A payload too large for one line, written to a file.
    BlobRef(BlobRef),
    /// Either side: something went wrong (e.g. `vulkan_unavailable`).
    Error(ErrorMessage),
    /// Producer → companion: the session ends; windows may stay open.
    Close(Close),
    /// A message type this build does not know. Readers skip it; it is
    /// never written.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// `{"type":"hello","protocol":"rustman-viz/1","producer":"rustman 0.2.0"}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: String,
    pub producer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol: String,
    pub viewer: String,
    /// The views the companion can open.
    #[serde(default)]
    pub views: Vec<View>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReject {
    /// The protocol names the companion speaks.
    pub supported: Vec<String>,
}

/// The companion's views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    RunDashboard,
    ExplorationDashboard,
    ApiGraph,
    ResponseInspector,
    /// The live load-test dashboard (`rustman bench --viz`).
    Bench,
    /// Load against latency and errors (`rustman stress --viz`): the
    /// stream is the bench's (`bench_tick` per second, then `close`).
    Stress,
    /// Uptime gauges, a latency plot and an outcome heatmap per monitor
    /// (`rustman monitor status --viz`, `v` in the TUI's Monitors view).
    MonitorTrend,
    /// A provider × method latency heatmap, block-time and gas plots and
    /// call-trace graphs (`rustman evm probe --viz`).
    ChainProbe,
    /// A view added by a later minor version.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenView {
    pub view: View,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStarted {
    /// RFC 3339 start time.
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// How many exchanges the run plans, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// How an exchange or a probe ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    /// A transport error (DNS, connect, TLS, timeout, ...): no status.
    Errored,
    Cancelled,
    Skipped,
    #[serde(other)]
    Unknown,
}

/// Assertion tally of one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Assertions {
    pub passed: u32,
    pub failed: u32,
}

/// One finished exchange of a run, already redacted by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeCompleted {
    /// The request id (workspace path without `.rman.yaml`).
    pub request: String,
    pub method: String,
    pub url: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub duration_ms: u64,
    #[serde(default)]
    pub assertions: Assertions,
    /// Why the request errored (a transport error such as `dns`,
    /// `connect` or `tls`, with its message), redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
}

/// The final state of a run or exploration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Completed,
    Aborted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunSummary {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    pub skipped: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFinished {
    pub state: RunState,
    pub summary: RunSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorationStarted {
    pub started_at: String,
    /// Operations in the API description.
    pub operations: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCompleted {
    /// The operation, e.g. `POST /users`.
    pub operation: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub duration_ms: u64,
}

/// Finding severities, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub operation: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorationFinished {
    pub state: RunState,
    pub operations_total: u64,
    pub operations_exercised: u64,
    #[serde(default)]
    pub status_codes_documented: u64,
    #[serde(default)]
    pub status_codes_seen: u64,
}

/// The kind of a saved report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    Run,
    Exploration,
    Bench,
    Stress,
    #[serde(other)]
    Unknown,
}

/// Open a saved report file in the matching view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadReport {
    pub path: String,
    pub kind: ReportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<View>,
}

/// One second of a bench. It holds counts, timings and VUs only, never
/// request or response content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchTick {
    /// 1 for the first second of the bench.
    pub second: u32,
    pub elapsed_ms: u64,
    /// How long load still runs, when the bench has a fixed duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Option<u64>,
    /// VUs running at the end of the second.
    pub active_vus: u32,
    /// Requests that finished in this second.
    pub requests: u64,
    /// Requests of this second that failed.
    pub errors: u64,
    pub rps: f64,
    /// Latency percentiles of this second, in milliseconds.
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

/// How one threshold of a bench ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchThreshold {
    /// As written, such as `p95<300ms`.
    pub expression: String,
    pub passed: bool,
    /// The measured value, such as `412ms`.
    pub actual: String,
}

/// The end of a bench: totals over the whole run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchFinished {
    pub state: RunState,
    pub duration_ms: u64,
    pub iterations: u64,
    pub requests: u64,
    pub errors: u64,
    pub rps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    #[serde(default)]
    pub thresholds: Vec<BenchThreshold>,
    /// Why the bench stopped early: `threshold` or `interrupted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<String>,
}

/// The trend of one monitor over the store's retention: counts, timings
/// and outcomes only, never request or response content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorTrend {
    pub monitor: String,
    pub environment: String,
    /// As shown by `rustman monitor list`, such as `every 5m`.
    pub schedule: String,
    /// Uptime gauges in percent; left out without executed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_24h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_7d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_30d: Option<f64>,
    /// The p95 latency of each hour with executed runs, oldest first.
    #[serde(default)]
    pub latency: Vec<LatencyPoint>,
    /// Outcomes by UTC hour of day, `hour` 0 to 23.
    #[serde(default)]
    pub hour_of_day: Vec<HourOutcomes>,
}

/// The latency of one method on one provider, in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeLatency {
    /// The provider's endpoint, redacted.
    pub provider: String,
    pub method: String,
    pub samples: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// One block as a provider reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBlock {
    pub provider: String,
    pub number: u64,
    /// Unix seconds.
    pub timestamp: u64,
    pub gas_used: u64,
    pub gas_limit: u64,
}

/// A call trace as a node graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTrace {
    pub provider: String,
    /// Frames in depth-first order; the first is the root.
    pub nodes: Vec<TraceNode>,
}

/// One frame of a call trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceNode {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// `CALL`, `STATICCALL`, `DELEGATECALL`, ...
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One point of the latency plot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyPoint {
    /// RFC 3339 start of the hour (UTC).
    pub hour: String,
    pub p95_ms: f64,
}

/// One cell row of the outcome heatmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HourOutcomes {
    /// The UTC hour of day, 0 to 23.
    pub hour: u8,
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    pub skipped: u64,
}

/// One response header, as received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// A response to show in the inspector. Header values and body are
/// redacted by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default)]
    pub headers: Vec<Header>,
    /// A JSONPath selecting the array to tabulate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    pub body: Body,
}

/// A body sent inline, or by reference when it does not fit on a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Inline { text: String },
    BlobRef(BlobRef),
}

/// A payload written to a temp file instead of the pipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub path: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMessage {
    /// A stable code, e.g. `vulkan_unavailable`.
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Close {}

impl Hello {
    /// The `hello` of this build's protocol, from `producer` (e.g.
    /// `rustman 0.2.0`).
    pub fn new(producer: impl Into<String>) -> Self {
        Self {
            protocol: crate::PROTOCOL.to_owned(),
            producer: producer.into(),
        }
    }
}

impl Message {
    /// The `type` tag, e.g. `hello_ack`; `None` for [`Message::Unknown`].
    pub fn kind(&self) -> Option<&'static str> {
        Some(match self {
            Self::Hello(_) => "hello",
            Self::HelloAck(_) => "hello_ack",
            Self::HelloReject(_) => "hello_reject",
            Self::OpenView(_) => "open_view",
            Self::RunStarted(_) => "run_started",
            Self::ExchangeCompleted(_) => "exchange_completed",
            Self::RunFinished(_) => "run_finished",
            Self::ExplorationStarted(_) => "exploration_started",
            Self::ProbeCompleted(_) => "probe_completed",
            Self::Finding(_) => "finding",
            Self::ExplorationFinished(_) => "exploration_finished",
            Self::LoadReport(_) => "load_report",
            Self::BenchTick(_) => "bench_tick",
            Self::BenchFinished(_) => "bench_finished",
            Self::MonitorTrend(_) => "monitor_trend",
            Self::ProbeLatency(_) => "probe_latency",
            Self::ChainBlock(_) => "chain_block",
            Self::CallTrace(_) => "call_trace",
            Self::InspectResponse(_) => "inspect_response",
            Self::BlobRef(_) => "blob_ref",
            Self::Error(_) => "error",
            Self::Close(_) => "close",
            Self::Unknown => return None,
        })
    }

    /// Messages a full queue must never drop (design D4): findings and the
    /// final events. Progress may be coalesced.
    pub fn is_priority(&self) -> bool {
        matches!(
            self,
            Self::Finding(_)
                | Self::RunFinished(_)
                | Self::ExplorationFinished(_)
                | Self::BenchFinished(_)
        )
    }
}
