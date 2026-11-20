//! What one monitor run stores (monitors "Result store"): one JSON line in
//! the day's raw file. The counts use the `summary` fields of the
//! `rustman.report/v1` run report (test-reports).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// How a monitor run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Every request passed.
    Pass,
    /// An assertion failed, and no request errored.
    Fail,
    /// A request errored or was blocked, or the run hit its deadline.
    Error,
    /// Not run: the previous run was still in progress.
    Skipped,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The counts of a run, as in a run report's `summary`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errored: usize,
    pub skipped: usize,
}

/// One request of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestResult {
    /// The request id (its path without `.rman.yaml`).
    pub id: String,
    /// `passed`, `failed`, `errored` or `skipped`.
    pub status: String,
    /// The response status code, when a response arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    pub duration_ms: f64,
}

/// One stored run. Messages are redacted before they get here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub at: Timestamp,
    pub outcome: Outcome,
    /// The whole run's latency.
    pub duration_ms: f64,
    #[serde(default)]
    pub summary: Counts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<RequestResult>,
    /// Failed assertions and request errors, as `<request id>: <message>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    /// Why the run errored as a whole, or was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl RunResult {
    /// A run skipped because the previous one was still going.
    pub fn skipped(at: Timestamp) -> Self {
        Self {
            at,
            outcome: Outcome::Skipped,
            duration_ms: 0.0,
            summary: Counts::default(),
            requests: Vec::new(),
            failures: Vec::new(),
            message: Some("skipped: previous run still in progress".to_owned()),
        }
    }
}
