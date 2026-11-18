//! The exploration report (`rustman.report/v1` with `kind: exploration`):
//! the seed and safety settings, probe totals by outcome, the coverage
//! matrix, the findings de-duplicated per rule and operation (with their
//! first evidence and reproduction), and the resources left behind.
//!
//! These are plain data. `rustman-explore` builds them from a finished
//! exploration, with every text already redacted; the renderers read them.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The `kind` of an exploration report.
pub const KIND_EXPLORATION: &str = "exploration";

/// How bad a finding is, most severe first.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub const ALL: [Self; 5] = [
        Self::Critical,
        Self::High,
        Self::Medium,
        Self::Low,
        Self::Info,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or_else(|| {
                format!("unknown severity '{s}' (expected critical, high, medium, low or info)")
            })
    }
}

/// A count of findings per severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SeverityCounts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
}

impl SeverityCounts {
    pub fn add(&mut self, severity: Severity) {
        *self.slot(severity) += 1;
    }

    pub fn get(&self, severity: Severity) -> usize {
        match severity {
            Severity::Critical => self.critical,
            Severity::High => self.high,
            Severity::Medium => self.medium,
            Severity::Low => self.low,
            Severity::Info => self.info,
        }
    }

    pub fn total(&self) -> usize {
        Severity::ALL.iter().map(|s| self.get(*s)).sum()
    }

    /// How many are at `threshold` or more severe.
    pub fn at_or_above(&self, threshold: Severity) -> usize {
        Severity::ALL
            .iter()
            .filter(|s| **s <= threshold)
            .map(|s| self.get(*s))
            .sum()
    }

    fn slot(&mut self, severity: Severity) -> &mut usize {
        match severity {
            Severity::Critical => &mut self.critical,
            Severity::High => &mut self.high,
            Severity::Medium => &mut self.medium,
            Severity::Low => &mut self.low,
            Severity::Info => &mut self.info,
        }
    }
}

impl fmt::Display for SeverityCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = Severity::ALL
            .iter()
            .map(|s| format!("{} {s}", self.get(*s)))
            .collect();
        f.write_str(&parts.join(", "))
    }
}

/// A probe family (`--probes`), in plan order.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    Conformance,
    Auth,
    NegativeInput,
    MethodNotAllowed,
    Lifecycle,
    Bola,
    Fuzz,
    UndocumentedStatus,
    ErrorLeakage,
    SecurityHeaders,
}

impl Family {
    pub const ALL: [Self; 10] = [
        Self::Conformance,
        Self::Auth,
        Self::NegativeInput,
        Self::MethodNotAllowed,
        Self::Lifecycle,
        Self::Bola,
        Self::Fuzz,
        Self::UndocumentedStatus,
        Self::ErrorLeakage,
        Self::SecurityHeaders,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conformance => "conformance",
            Self::Auth => "auth",
            Self::NegativeInput => "negative-input",
            Self::MethodNotAllowed => "method-not-allowed",
            Self::Lifecycle => "lifecycle",
            Self::Bola => "bola",
            Self::Fuzz => "fuzz",
            Self::UndocumentedStatus => "undocumented-status",
            Self::ErrorLeakage => "error-leakage",
            Self::SecurityHeaders => "security-headers",
        }
    }

    /// Every family but `bola`, which needs a second identity.
    pub fn defaults() -> BTreeSet<Self> {
        Self::ALL.into_iter().filter(|f| *f != Self::Bola).collect()
    }

    /// Families that scan every observed response rather than planning
    /// probes of their own.
    pub fn is_scan(self) -> bool {
        matches!(
            self,
            Self::UndocumentedStatus | Self::ErrorLeakage | Self::SecurityHeaders
        )
    }

    /// A comma-separated `--probes` list.
    pub fn parse_list(list: &str) -> Result<BTreeSet<Self>, String> {
        list.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .collect()
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Family {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|f| f.as_str()).collect();
                format!(
                    "unknown probe family '{s}' (expected one of {})",
                    names.join(", ")
                )
            })
    }
}

/// Everything a resume repeats: the probe selection, the seed, the safety
/// flags and the budgets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Settings {
    pub probes: Vec<Family>,
    #[serde(default)]
    pub operations: Vec<String>,
    pub seed: u64,
    pub fuzz_cases: usize,
    pub allow_mutations: bool,
    pub allow_destructive: bool,
    /// Extra origins (`--allow-host`), as `scheme://host[:port]`.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// `--production <host>`.
    #[serde(default)]
    pub production: Option<String>,
    #[serde(default)]
    pub allow_remote_refs: bool,
    pub rate: u32,
    pub concurrency: u32,
    pub max_probes: u64,
    pub max_duration_ms: u64,
    pub fail_on: Severity,
}

/// A part of a whole, with its percentage (one decimal).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ratio {
    pub covered: usize,
    pub total: usize,
    pub percent: f64,
}

impl Ratio {
    pub fn new(covered: usize, total: usize) -> Self {
        let percent = (covered * 1000 + total / 2)
            .checked_div(total)
            .map_or(0.0, |tenths| {
                f64::from(u32::try_from(tenths).unwrap_or(u32::MAX)) / 10.0
            });
        Self {
            covered,
            total,
            percent,
        }
    }
}

/// The state of one documented status in the coverage matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Cell {
    Observed,
    NotObserved,
    /// Nothing was sent for the operation: the safety policy refused it.
    Blocked,
}

/// One documented status of one operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusCell {
    /// As documented: `200`, `4XX` or `default`.
    pub status: String,
    pub state: Cell,
}

/// One operation's row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Row {
    pub operation: String,
    pub statuses: Vec<StatusCell>,
}

/// Operations by documented statuses.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Coverage {
    /// Operations that answered at least once.
    pub operations: Ratio,
    /// Documented statuses observed.
    pub statuses: Ratio,
    pub matrix: Vec<Row>,
}

/// The redacted exchange behind a finding.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub method: String,
    pub url: String,
    pub request_headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    /// The start of the response body (at most 4 KiB).
    pub body: String,
    /// The body was longer than the excerpt.
    #[serde(default)]
    pub truncated: bool,
}

/// How to reproduce one finding.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reproduction {
    /// The evidence request as a curl command; secrets are `{{name}}`.
    pub curl: String,
    /// A request file (YAML) sending the evidence request to `{{baseUrl}}`
    /// and asserting what a correct API answers.
    pub request: String,
}

/// A resource a probe created, identified by a captured id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Resource {
    /// The probe that created it.
    pub probe: String,
    /// `METHOD /path` of the create operation.
    pub operation: String,
    /// Its path relative to the target, as a template (`/users/42`).
    pub path: String,
    /// Its URL, redacted, as reports show it.
    pub url: String,
    /// `METHOD /path` of the documented operation that deletes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

/// One deletion of a created resource, after the probes ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cleanup {
    /// The resource's path, as [`Resource::path`].
    pub path: String,
    /// Its URL, redacted.
    pub url: String,
    /// The status the delete answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Why no response came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Cleanup {
    /// The resource is gone: the delete answered 2xx, 404 or 410.
    pub fn deleted(&self) -> bool {
        self.status
            .is_some_and(|s| (200..300).contains(&s) || s == 404 || s == 410)
    }
}

/// Probes by how they ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeTotals {
    pub total: usize,
    pub passed: usize,
    /// Probes that produced at least one finding.
    pub finding: usize,
    pub blocked: usize,
    pub skipped: usize,
    pub not_run: usize,
    /// Probes whose request failed or whose chain stopped early, without a
    /// finding.
    pub failed: usize,
}

/// The counts of an exploration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "ExplorationSummary")]
pub struct Summary {
    pub probes: ProbeTotals,
    /// Distinct findings by severity.
    pub findings: SeverityCounts,
    /// Requests sent, cleanup included.
    pub requests: usize,
    /// Time spent executing.
    pub duration_ms: u64,
}

/// One finding, de-duplicated per rule and operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportFinding {
    /// `f001`, `f002`, ... by severity.
    pub id: String,
    pub rule: String,
    pub severity: Severity,
    pub operation: String,
    pub description: String,
    /// How to fix it, from the rule registry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remediation: String,
    /// The first occurrence's message.
    pub message: String,
    pub occurrences: usize,
    /// The probes it occurred in, in plan order.
    pub probes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_pointer: Option<String>,
    /// The first occurrence's exchange.
    pub evidence: Evidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<Reproduction>,
}

/// One probe and how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeResult {
    pub id: String,
    pub family: Family,
    pub operation: String,
    pub method: String,
    pub variant: String,
    /// `passed`, `finding`, `blocked`, `skipped`, `not run` or `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One documented operation: how reports group and locate it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationInfo {
    /// `METHOD /path`.
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Its tags, in document order; the first groups it in JUnit.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `report.json` of an exploration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplorationReport {
    /// `rustman.report/v1`.
    pub schema: String,
    /// `exploration`.
    pub kind: String,
    pub rustman_version: String,
    pub id: String,
    /// RFC 3339.
    pub started_at: String,
    /// RFC 3339.
    pub finished_at: String,
    #[serde(default)]
    pub environment: Option<String>,
    pub target: String,
    /// The OpenAPI document, as a resume finds it.
    pub spec: String,
    pub document_sha256: String,
    pub seed: u64,
    /// The probe selection, safety flags and budgets.
    pub settings: Settings,
    pub summary: Summary,
    pub coverage: Coverage,
    pub findings: Vec<ReportFinding>,
    pub probes: Vec<ProbeResult>,
    /// Created resources that were not deleted, oldest first.
    pub left_behind: Vec<Resource>,
    /// Deletions of created resources, in the order they ran.
    #[serde(default)]
    pub cleanup: Vec<Cleanup>,
    /// The documented operations, in document order.
    #[serde(default)]
    pub operations: Vec<OperationInfo>,
}

impl ExplorationReport {
    /// Distinct findings at or above `threshold`.
    pub fn failing(&self, threshold: Severity) -> usize {
        self.summary.findings.at_or_above(threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_round_to_one_decimal() {
        assert_eq!(Ratio::new(8, 10).percent, 80.0);
        assert_eq!(Ratio::new(15, 25).percent, 60.0);
        assert_eq!(Ratio::new(1, 3).percent, 33.3);
        assert_eq!(Ratio::new(2, 3).percent, 66.7);
        assert_eq!(Ratio::new(0, 0).percent, 0.0);
    }

    #[test]
    fn family_lists_parse() {
        let set = Family::parse_list("conformance, auth").unwrap();
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            [Family::Conformance, Family::Auth]
        );
        assert!(
            Family::parse_list("conformance,nope")
                .unwrap_err()
                .contains("'nope'")
        );
        assert!(!Family::defaults().contains(&Family::Bola));
        assert_eq!(Family::defaults().len(), 9);
    }

    #[test]
    fn severity_counts_threshold() {
        let mut counts = SeverityCounts::default();
        counts.add(Severity::Critical);
        counts.add(Severity::Medium);
        counts.add(Severity::Info);
        assert_eq!(counts.at_or_above(Severity::High), 1);
        assert_eq!(counts.at_or_above(Severity::Medium), 2);
        assert_eq!(counts.total(), 3);
        assert_eq!("low".parse::<Severity>(), Ok(Severity::Low));
        assert!("nope".parse::<Severity>().is_err());
    }
}
