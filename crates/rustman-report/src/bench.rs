//! The bench report (`rustman.report/v1` with `kind: bench`): what a
//! `rustman bench` load test measured. It holds the load profile, the
//! target, latency and error statistics per request file and overall, a
//! per-second time series, the threshold results and a few error samples
//! per error class.
//!
//! These are plain data. `rustman-bench` builds them with every text
//! already redacted (error samples go through the run's `Redactor`); the
//! renderers read them. Its statistics types are also what `rustman-bench`
//! reports live, so the report and the terminal summary cannot drift.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The `kind` of a bench report.
pub const KIND_BENCH: &str = "bench";

/// The most error samples a report keeps per error class.
pub const MAX_SAMPLES_PER_CLASS: usize = 20;

/// A bench report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BenchReport {
    /// `rustman.report/v1`.
    pub schema: String,
    /// `bench`.
    pub kind: String,
    pub rustman_version: String,
    /// RFC 3339.
    pub started_at: String,
    /// RFC 3339.
    pub finished_at: String,
    /// The `--target` origin: the only one requests could go to.
    pub target: String,
    pub environment: Option<String>,
    pub profile: BenchProfile,
    /// Why the bench stopped early; absent when it ran to its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<Aborted>,
    pub totals: BenchTotals,
    /// Every request file together.
    pub overall: StatsSummary,
    /// One entry per request file, in collection order.
    pub files: Vec<FileSummary>,
    /// One point per second of the run.
    pub series: Vec<BenchSecond>,
    /// Every threshold, in the order given.
    pub thresholds: Vec<ThresholdResult>,
    /// At most 20 per error class, in the order they happened (redacted).
    pub error_samples: Vec<ErrorSample>,
    /// Non-fatal problems, such as a generator-bound run.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl BenchReport {
    /// Whether every threshold passed and no threshold aborted the run.
    pub fn passed(&self) -> bool {
        self.aborted != Some(Aborted::Threshold) && self.thresholds.iter().all(|t| t.passed)
    }
}

/// Why a bench stopped before its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Aborted {
    /// An abort-on-fail (`!`) threshold was breached.
    Threshold,
    /// Ctrl+C.
    Interrupted,
}

impl Aborted {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for Aborted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The load profile, as the flags gave it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BenchProfile {
    /// `constant-vus`, `ramping-vus` or `constant-arrival-rate`.
    pub executor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u64>,
    /// `0`, `500ms` or a range such as `1s..3s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_time: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageProfile>,
    /// Iterations started per second (`constant-arrival-rate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_vus: Option<u32>,
}

/// One `ramping-vus` stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StageProfile {
    pub duration_ms: u64,
    pub target: u32,
}

/// Whether latencies include queuing before the iteration started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LatencyKind {
    /// Closed models: the time each request took.
    Service,
    /// The open model: measured from the scheduled start (coordinated
    /// omission corrected).
    Corrected,
}

impl LatencyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Corrected => "corrected",
        }
    }
}

/// The counts of a whole bench.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BenchTotals {
    pub latency_kind: LatencyKind,
    pub duration_ms: f64,
    /// Iterations that ran to their end.
    pub iterations: u64,
    /// How long those iterations took, from their first request's start to
    /// their last one's end (think time excluded).
    #[serde(default)]
    pub iteration_duration: LatencySummary,
    /// Iterations cancelled before their end.
    pub interrupted: u64,
    /// Iterations the open model scheduled (0 for closed models).
    pub scheduled: u64,
    /// Scheduled iterations that found no free VU.
    pub dropped: u64,
    /// Requests per second over the whole run.
    pub rps: f64,
    /// The 99th percentile of the load generator's scheduling lag: how
    /// late its timers fired. Above 10 ms the results may be
    /// generator-bound.
    pub generator_lag_p99_ms: f64,
}

/// The statistics of one request file, or of every file together.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatsSummary {
    pub requests: u64,
    /// Errors by class: `4xx`, `5xx`, `timeout`, `connect`, `tls`,
    /// `blocked`, `assertion`, `other`.
    pub errors: BTreeMap<String, u64>,
    /// Errors over requests, from 0 to 1.
    pub error_rate: f64,
    pub latency: LatencySummary,
    /// Assertions that passed and failed.
    pub checks: Checks,
    /// Average time of each phase, over the requests that had it.
    pub phases: PhaseSummary,
    /// Approximate: the request line, headers and text body as recorded.
    pub bytes_sent: u64,
    /// Response body bytes.
    pub bytes_received: u64,
}

impl StatsSummary {
    /// Errors of the class named `class`.
    pub fn errors_named(&self, class: &str) -> u64 {
        self.errors.get(class).copied().unwrap_or(0)
    }
}

/// Assertion outcomes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Checks {
    pub passed: u64,
    pub failed: u64,
}

impl Checks {
    /// The pass rate from 0 to 1; `None` without assertions.
    pub fn rate(&self) -> Option<f64> {
        let total = self.passed + self.failed;
        (total > 0).then(|| self.passed as f64 / total as f64)
    }
}

/// Latency percentiles in milliseconds, from an HDR histogram with 3
/// significant digits; all 0 without requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LatencySummary {
    pub min_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Average phase timings in milliseconds; a phase no request went
/// through (no new connection, no TLS) is absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhaseSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_ms: Option<f64>,
}

/// The statistics of one request file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileSummary {
    /// The request file, relative to the workspace root, with `/`.
    pub file: String,
    #[serde(flatten)]
    pub stats: StatsSummary,
}

/// One second of a bench.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BenchSecond {
    /// 1 for the first second of the run.
    pub second: u32,
    pub requests: u64,
    pub errors: u64,
    pub rps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// VUs running at the end of the second.
    pub active_vus: u32,
}

/// How one threshold ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThresholdResult {
    /// As written, such as `p95<300ms` or `error_rate<5%!`.
    pub expression: String,
    pub passed: bool,
    /// The measured value, such as `412ms` or `0.20%`; `n/a` when there
    /// was nothing to measure (`checks` without assertions).
    pub actual: String,
    /// An abort-on-fail (`!`) threshold.
    pub abort: bool,
}

impl ThresholdResult {
    /// `✓ p95<300ms (212ms)` or `✗ p95<300ms (412ms)`.
    pub fn line(&self) -> String {
        let mark = if self.passed { '✓' } else { '✗' };
        format!("{mark} {} ({})", self.expression, self.actual)
    }
}

/// One error, as it happened (redacted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorSample {
    /// The error class: `4xx`, `5xx`, `timeout`, ...
    pub class: String,
    /// The request file.
    pub file: String,
    pub message: String,
    /// Milliseconds since the start of the run.
    pub at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_lines_mark_breaches() {
        let mut result = ThresholdResult {
            expression: "p95<300ms".into(),
            passed: false,
            actual: "412ms".into(),
            abort: false,
        };
        assert_eq!(result.line(), "✗ p95<300ms (412ms)");
        result.passed = true;
        assert_eq!(result.line(), "✓ p95<300ms (412ms)");
    }

    #[test]
    fn checks_have_no_rate_without_assertions() {
        assert_eq!(Checks::default().rate(), None);
        let checks = Checks {
            passed: 3,
            failed: 1,
        };
        assert_eq!(checks.rate(), Some(0.75));
    }
}
