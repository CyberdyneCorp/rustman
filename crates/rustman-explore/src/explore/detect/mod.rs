//! Detectors: what a probe step's response says about the target. Each
//! returns rule ids from the [registry](super::rules) with a message;
//! [`super::exec`] adds the severity, the operation and the redacted
//! evidence.

mod checks;
mod conformance;
mod headers;
mod leakage;

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use conformance::{documented as documented_response, schema_for as conformance_schema};
pub use leakage::{LEAKAGE_PATTERNS_VERSION, MAX_SCANNED_BYTES, leaks};

use super::plan::{Check, Family};
use super::repro::Reproduction;
use super::rules::Severity;
use crate::openapi::model::Operation;

/// Evidence bodies are cut at this many bytes.
pub const EVIDENCE_BYTES: usize = 4096;

/// A rule a response broke, before evidence is attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub rule: &'static str,
    pub message: String,
    /// The instance and schema pointers of a schema violation.
    pub pointers: Option<(String, String)>,
}

impl Detected {
    pub fn new(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            rule,
            message: message.into(),
            pointers: None,
        }
    }
}

pub use rustman_report::exploration::Evidence;

/// One finding of one probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    /// `METHOD /path` of the operation (or of the undocumented method).
    pub operation: String,
    pub probe: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_pointer: Option<String>,
    pub evidence: Evidence,
    /// How to reproduce it: a curl command and a regression request file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<Reproduction>,
}

/// A response as the detectors see it.
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    pub status: u16,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
    /// The response came over HTTPS.
    pub https: bool,
}

impl Observed<'_> {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(self.body).ok()
    }
}

/// What a step's response is judged against.
#[derive(Clone, Copy, Debug)]
pub struct Scope<'a> {
    /// The documented operation, `None` for an undocumented method.
    pub operation: Option<&'a Operation>,
    pub components: &'a Value,
    /// The enabled scan families.
    pub scans: &'a [Family],
    /// The previous step's response body, for checks that compare.
    pub previous: Option<&'a [u8]>,
}

/// Everything `response` to a step with `check` breaks.
pub fn detect(check: Option<&Check>, scope: &Scope<'_>, response: &Observed<'_>) -> Vec<Detected> {
    let mut found = Vec::new();
    if let Some(check) = check {
        found.extend(checks::check(check, scope, response));
    }
    if scope.scans.contains(&Family::UndocumentedStatus) {
        found.extend(conformance::undocumented_status(scope.operation, response));
    }
    if scope.scans.contains(&Family::ErrorLeakage) {
        found.extend(
            leaks(response.body).into_iter().map(|(label, excerpt)| {
                Detected::new("error-leakage", format!("{label}: {excerpt}"))
            }),
        );
    }
    if scope.scans.contains(&Family::SecurityHeaders) {
        found.extend(headers::missing(response));
    }
    found
}
