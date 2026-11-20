//! The contract report (`rustman.report/v1` with `kind: contract`): what a
//! `rustman contract` command checked, as one list of items whatever the
//! mode (an OpenAPI diff, a Pact verification or a passive conformance
//! check), so every renderer handles every mode.
//!
//! These are plain data. `rustman-contract` builds them with every text
//! already redacted; the renderers read them.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The `kind` of a contract report.
pub const KIND_CONTRACT: &str = "contract";

/// Which contract command wrote the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ContractMode {
    /// `rustman contract diff`: differences between two OpenAPI documents.
    Diff,
    /// `rustman contract pact verify`: Pact interactions replayed against a
    /// provider.
    PactVerify,
    /// `rustman contract verify`: recorded exchanges checked against a
    /// document.
    Conformance,
}

impl ContractMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::PactVerify => "pact-verify",
            Self::Conformance => "conformance",
        }
    }
}

impl fmt::Display for ContractMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A contract report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractReport {
    /// `rustman.report/v1`.
    pub schema: String,
    /// `contract`.
    pub kind: String,
    pub rustman_version: String,
    pub mode: ContractMode,
    /// The documents and files checked, such as a diff's `old` and `new`.
    pub sources: Vec<ContractSource>,
    /// The rule catalog version a diff classified with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<u32>,
    pub summary: ContractSummary,
    /// In the command's order (for a diff, the document order).
    pub items: Vec<ContractItem>,
    /// Non-fatal problems, such as expired or unused allowlist entries.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// For a conformance check: the documented operations the recorded
    /// traffic exercised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ContractCoverage>,
}

/// Which documented operations recorded traffic exercised (passive
/// conformance).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractCoverage {
    /// Documented operations with at least one exchange.
    pub observed: usize,
    /// Documented operations.
    pub total: usize,
    /// Every documented operation, in document order.
    pub operations: Vec<OperationCoverage>,
}

impl ContractCoverage {
    /// `5/12 operations observed`.
    pub fn line(&self) -> String {
        format!("{}/{} operations observed", self.observed, self.total)
    }
}

/// One documented operation and the exchanges that matched it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationCoverage {
    /// `METHOD /path`.
    pub operation: String,
    pub exchanges: usize,
    /// Exchanges of this operation with a violation.
    pub violations: usize,
}

/// One document or file a contract command read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractSource {
    /// What it is to the command: `old`, `new`, `spec`, `pact`, `history`.
    pub role: String,
    /// Its path, URL or `<git-ref>:<path>`.
    pub name: String,
}

/// The counts of a contract report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractSummary {
    /// Items per status, in the mode's order (for a diff: breaking,
    /// non-breaking, unknown), including statuses with no item.
    pub counts: Vec<StatusCount>,
    /// Items an allowlist accepted.
    pub allowed: usize,
    /// Items that fail the command.
    pub failing: usize,
    /// `true` when no item fails the command.
    pub passed: bool,
}

impl ContractSummary {
    /// `2 breaking, 1 non-breaking, 0 unknown`, with `, 1 allowed` when an
    /// allowlist accepted any.
    pub fn line(&self) -> String {
        let mut parts: Vec<String> = self
            .counts
            .iter()
            .map(|c| format!("{} {}", c.count, c.status))
            .collect();
        if self.allowed > 0 {
            parts.push(format!("{} allowed", self.allowed));
        }
        parts.join(", ")
    }
}

/// How many items have one status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusCount {
    pub status: String,
    pub count: usize,
}

/// One difference, interaction or exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractItem {
    /// The rule id, e.g. `operation-removed`.
    pub rule: String,
    /// The rule's one-line description.
    pub description: String,
    /// The classification or verdict, e.g. `breaking`.
    pub status: String,
    /// The allowlist entry's justification, when one accepted the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<String>,
    /// Whether the item fails the command.
    pub failing: bool,
    /// The operation (`METHOD /path`), interaction or exchange.
    pub subject: String,
    pub message: String,
    /// Where the item is, the primary location first (for a diff, the new
    /// document).
    pub locations: Vec<ContractLocation>,
}

impl ContractItem {
    /// `breaking (allowed)` for an allowed item, the status otherwise.
    pub fn label(&self) -> String {
        if self.allowed.is_some() {
            format!("{} (allowed)", self.status)
        } else {
            self.status.clone()
        }
    }
}

/// A place in one of the report's sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractLocation {
    /// The source's role, e.g. `new`.
    pub role: String,
    /// The source's name (path, URL or `<git-ref>:<path>`).
    pub document: String,
    /// A JSON pointer into it.
    pub pointer: String,
}

impl ContractReport {
    /// The items grouped by status, in the order of the summary's counts;
    /// failing items first within a group.
    pub fn groups(&self) -> Vec<(&str, Vec<&ContractItem>)> {
        self.summary
            .counts
            .iter()
            .map(|c| {
                let mut items: Vec<&ContractItem> =
                    self.items.iter().filter(|i| i.status == c.status).collect();
                items.sort_by_key(|i| !i.failing);
                (c.status.as_str(), items)
            })
            .filter(|(_, items)| !items.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(status: &str, failing: bool) -> ContractItem {
        ContractItem {
            rule: "r".into(),
            description: "d".into(),
            status: status.into(),
            allowed: None,
            failing,
            subject: "GET /".into(),
            message: "m".into(),
            locations: Vec::new(),
        }
    }

    #[test]
    fn groups_follow_the_counts_with_failing_items_first() {
        let report = ContractReport {
            schema: crate::SCHEMA.into(),
            kind: KIND_CONTRACT.into(),
            rustman_version: "0".into(),
            mode: ContractMode::Diff,
            sources: Vec::new(),
            catalog_version: Some(1),
            summary: ContractSummary {
                counts: vec![
                    StatusCount {
                        status: "breaking".into(),
                        count: 2,
                    },
                    StatusCount {
                        status: "unknown".into(),
                        count: 0,
                    },
                ],
                allowed: 1,
                failing: 1,
                passed: false,
            },
            items: vec![item("breaking", false), item("breaking", true)],
            warnings: Vec::new(),
            coverage: None,
        };
        let groups = report.groups();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].1[0].failing);
        assert_eq!(report.summary.line(), "2 breaking, 0 unknown, 1 allowed");
        let mut allowed = item("breaking", false);
        allowed.allowed = Some("ok".into());
        assert_eq!(allowed.label(), "breaking (allowed)");
        assert_eq!(ContractMode::PactVerify.to_string(), "pact-verify");
    }
}
