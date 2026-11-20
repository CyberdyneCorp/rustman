//! The stress report (`rustman.report/v1` with `kind: stress`, test-reports
//! "Stress report content"): the mode, profile, seed, target and SLOs; the
//! bench time series with valid and fuzzed traffic apart; the step table;
//! the breakpoint, rate limiting, kill and recovery; and the findings in
//! the exploration finding format, so SARIF, Markdown, HTML and `report
//! diff` read them like exploration findings.
//!
//! These are plain data. `rustman-bench` builds them with every text
//! already redacted; the renderers read them.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bench::{BenchSecond, FileSummary, StatsSummary};
use crate::exploration::{ReportFinding, Severity, SeverityCounts};

/// The `kind` of a stress report.
pub const KIND_STRESS: &str = "stress";

/// A stress report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StressReport {
    /// `rustman.report/v1`.
    pub schema: String,
    /// `stress`.
    pub kind: String,
    pub rustman_version: String,
    /// `str-<date>-<hex>`: what `rustman stress export` takes.
    pub id: String,
    /// RFC 3339.
    pub started_at: String,
    /// RFC 3339.
    pub finished_at: String,
    /// The `--target` origin.
    pub target: String,
    pub environment: Option<String>,
    /// `breakpoint`, `spike`, `soak` or `fuzz`.
    pub mode: String,
    pub profile: StressProfile,
    /// The seed of fuzzed slots and variants.
    pub seed: u64,
    /// The SLOs in force, as written.
    pub slos: Vec<String>,
    /// Valid traffic, every request file together.
    pub overall: StatsSummary,
    /// Valid traffic per request file, in collection order.
    pub files: Vec<FileSummary>,
    /// Fuzzed traffic, measured apart; absent without fuzzing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzzed: Option<StatsSummary>,
    /// Valid traffic, one point per tick (a second).
    pub series: Vec<BenchSecond>,
    /// Fuzzed traffic, one point per tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fuzzed_series: Vec<BenchSecond>,
    /// The steps (a breakpoint's, or stretches at one rate).
    pub steps: Vec<StressStep>,
    /// A breakpoint run's result; absent for the other modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakpoint: Option<BreakpointSection>,
    /// The first rate, in requests per second, where the target
    /// throttled without breaking.
    pub rate_limited_at: Option<f64>,
    /// Set when the kill switch stopped the load.
    pub killed: Option<Killed>,
    pub recovery: RecoverySection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spike: Option<SpikeSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soak: Option<SoakSection>,
    /// What each wire-fault family got back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wire: Vec<WireSection>,
    pub summary: StressSummary,
    /// By severity, then rule.
    pub findings: Vec<StressFinding>,
    /// Windows where the client lagged behind its schedule.
    #[serde(default)]
    pub client_saturated: bool,
    #[serde(default)]
    pub interrupted: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl StressReport {
    /// Distinct findings at or above `threshold`.
    pub fn failing(&self, threshold: Severity) -> usize {
        self.summary.findings.at_or_above(threshold)
    }
}

/// The profile, as the flags gave it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StressProfile {
    /// Every value of the mode, such as `start_rps`, `step_factor`,
    /// `step_duration_ms`, `refine`, `rps`, `duration_ms`.
    pub settings: BTreeMap<String, f64>,
    /// `--fuzz-ratio`, when the run fuzzed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzz_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub malformed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wire: Vec<String>,
    /// The health request file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

/// One row of the step table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StressStep {
    /// Requests per second offered.
    pub offered_rps: f64,
    pub achieved_rps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub error_rate: f64,
    /// Errors by class (`5xx`, `timeout`, `connect`, ...).
    pub errors: BTreeMap<String, u64>,
    /// `ok`, `broken`, `rate-limited`, `client-saturated` or `unfinished`.
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_mode: Option<String>,
    /// Since load started, in milliseconds.
    pub start_ms: u64,
    pub end_ms: u64,
}

/// A breakpoint run's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BreakpointSection {
    /// The highest rate that held, in requests per second.
    pub max_sustainable_rps: Option<f64>,
    /// The lowest rate that broke.
    pub breaking_rps: Option<f64>,
    /// How the `breaking_rps` step broke.
    pub failure_mode: Option<String>,
    /// The request file that degraded first in that step.
    pub first_failing_endpoint: Option<FailingEndpoint>,
    /// `refined`, `cap-reached`, `broke-at-start`, `client-saturated`,
    /// `max-duration`, `interrupted` or `killed`.
    pub stop: String,
    /// The `max_rps` cap.
    pub max_rps: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FailingEndpoint {
    pub file: String,
    pub p95_ms: f64,
    pub error_rate: f64,
}

/// The kill switch stopped the load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Killed {
    /// The offered rate when the target went down.
    pub rps: f64,
    /// Why it was declared down.
    pub reason: String,
    /// `killed: target down at <rps> req/s (<reason>)`.
    pub message: String,
}

/// The recovery check after the load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecoverySection {
    /// `recovered`, `not-recovered`, `skipped` or `cancelled`.
    pub status: String,
    /// From the end of load to the first answer of the good streak.
    pub after_ms: Option<u64>,
    /// The health request's pre-stress latency.
    pub baseline_ms: Option<f64>,
    pub health: Option<String>,
    /// Why it was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub last_status: Option<u16>,
    pub last_latency_ms: Option<f64>,
    /// Every health request, in order.
    pub timeline: Vec<RecoveryProbe>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecoveryProbe {
    /// Since load stopped.
    pub offset_ms: u64,
    pub status: Option<u16>,
    pub latency_ms: f64,
    /// It counted toward recovery.
    pub good: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpikeSection {
    pub baseline_p95_ms: f64,
    pub spike_requests: u64,
    pub spike_errors: u64,
    pub peak_p95_ms: f64,
    pub recovered_after_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SoakSection {
    /// One per trend bucket (a minute).
    pub p95_ms: Vec<f64>,
    pub error_rates: Vec<f64>,
    pub baseline_p95_ms: f64,
    /// Milliseconds per hour.
    pub p95_slope: f64,
    /// Error-rate change per hour.
    pub error_slope: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WireSection {
    pub family: String,
    /// What each request got: a status, or what happened instead.
    pub outcomes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StressSummary {
    /// Distinct findings by severity.
    pub findings: SeverityCounts,
    pub requests: u64,
    pub fuzzed_requests: u64,
    pub duration_ms: u64,
}

/// A finding: the exploration finding (its `operation` is the request
/// file, or `wire <family>`), plus what stress adds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StressFinding {
    #[serde(flatten)]
    pub finding: ReportFinding,
    /// The fuzz source or fault family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// What replaying the input alone found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimized: Option<MinimizedInput>,
}

/// The minimized input of a crash-like finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MinimizedInput {
    /// The smallest body that still failed (redacted); absent when the
    /// input could not be reduced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub replays: u32,
    /// It did not fail alone: only under load.
    pub load_dependent: bool,
}
