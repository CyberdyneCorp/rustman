//! Contract reports (contract-testing "Contract reports"): a diff, a Pact
//! verification or a conformance check as a `rustman.report/v1` report
//! with `kind: contract` (`mode: diff | pact-verify | conformance`),
//! redacted before any renderer sees it.

use rustman_core::Redactor;
use rustman_report::SCHEMA;
use rustman_report::contract::{
    ContractItem, ContractLocation, ContractMode, ContractReport, ContractSource, ContractSummary,
    KIND_CONTRACT, StatusCount,
};

use crate::catalog::Classification;
use crate::conformance::{self, Conformance};
use crate::difference::Difference;
use crate::pact::verify::{Outcome, Verification, Verified};
use crate::verdict::Verdict;

const CLASSIFICATIONS: [Classification; 3] = [
    Classification::Breaking,
    Classification::NonBreaking,
    Classification::Unknown,
];

/// `verdict` as a contract report, with every text passed through
/// `redactor`.
pub fn diff_report(verdict: &Verdict, redactor: &Redactor) -> ContractReport {
    let diff = &verdict.diff;
    let items: Vec<ContractItem> = verdict
        .items()
        .map(|(d, allowed)| item(verdict, d, allowed))
        .collect();
    let failing = verdict.failing();
    let report = ContractReport {
        schema: SCHEMA.to_owned(),
        kind: KIND_CONTRACT.to_owned(),
        rustman_version: rustman_core::VERSION.to_owned(),
        mode: ContractMode::Diff,
        sources: vec![source("old", &diff.old), source("new", &diff.new)],
        catalog_version: Some(diff.catalog_version),
        summary: ContractSummary {
            counts: CLASSIFICATIONS
                .iter()
                .map(|c| StatusCount {
                    status: c.as_str().to_owned(),
                    count: diff.count(*c),
                })
                .collect(),
            allowed: verdict.allowed_count(),
            failing,
            passed: failing == 0,
        },
        items,
        warnings: verdict.warnings.clone(),
        coverage: None,
    };
    redacted(report, redactor)
}

fn source(role: &str, name: &str) -> ContractSource {
    ContractSource {
        role: role.to_owned(),
        name: name.to_owned(),
    }
}

fn item(verdict: &Verdict, d: &Difference, allowed: Option<&str>) -> ContractItem {
    let location = |role: &str, document: &str, pointer: &str| ContractLocation {
        role: role.to_owned(),
        document: document.to_owned(),
        pointer: pointer.to_owned(),
    };
    ContractItem {
        rule: d.rule.id().to_owned(),
        description: d.rule.info().description.to_owned(),
        status: d.classification.as_str().to_owned(),
        allowed: allowed.map(str::to_owned),
        failing: verdict.fails(d, allowed.is_some()),
        subject: d.operation.clone(),
        message: d.message.clone(),
        locations: vec![
            location("new", &verdict.diff.new, &d.new_pointer),
            location("old", &verdict.diff.old, &d.old_pointer),
        ],
    }
}

fn header(mode: ContractMode, sources: Vec<ContractSource>) -> ContractReport {
    ContractReport {
        schema: SCHEMA.to_owned(),
        kind: KIND_CONTRACT.to_owned(),
        rustman_version: rustman_core::VERSION.to_owned(),
        mode,
        sources,
        catalog_version: None,
        summary: ContractSummary::default(),
        items: Vec::new(),
        warnings: Vec::new(),
        coverage: None,
    }
}

fn summary(statuses: &[&str], items: &[ContractItem]) -> ContractSummary {
    let failing = items.iter().filter(|i| i.failing).count();
    ContractSummary {
        counts: statuses
            .iter()
            .map(|status| StatusCount {
                status: (*status).to_owned(),
                count: items.iter().filter(|i| i.status == *status).count(),
            })
            .collect(),
        allowed: 0,
        failing,
        passed: failing == 0,
    }
}

/// A Pact verification as a `mode: pact-verify` report: one item per
/// interaction (`passed`, `failed` or `blocked`).
pub fn pact_report(verification: &Verification, redactor: &Redactor) -> ContractReport {
    let mut files: Vec<&str> = verification
        .results
        .iter()
        .map(|r| r.file.as_str())
        .collect();
    files.dedup();
    let sources = files.into_iter().map(|f| source("pact", f)).collect();
    let mut report = header(ContractMode::PactVerify, sources);
    report.items = verification.results.iter().map(interaction_item).collect();
    report.summary = summary(&["passed", "failed", "blocked"], &report.items);
    report.warnings = verification.warnings.clone();
    redacted(report, redactor)
}

fn interaction_item(verified: &Verified) -> ContractItem {
    let message = match &verified.outcome {
        Outcome::Passed => "passed".to_owned(),
        Outcome::Failed(mismatches) => mismatches.join("; "),
        Outcome::Blocked(reason) => reason.clone(),
    };
    ContractItem {
        rule: "pact-interaction".to_owned(),
        description: "A Pact interaction replayed against the provider".to_owned(),
        status: verified.outcome.status().to_owned(),
        allowed: None,
        failing: verified.outcome != Outcome::Passed,
        subject: format!("{} ({})", verified.description, verified.consumer),
        message,
        locations: vec![ContractLocation {
            role: "pact".to_owned(),
            document: verified.file.clone(),
            pointer: format!("/interactions/{}", verified.index),
        }],
    }
}

/// What a conformance report names.
#[derive(Clone, Copy, Debug)]
pub struct ConformanceSources<'a> {
    /// The OpenAPI document.
    pub spec: &'a str,
    /// `history` or the run report's path.
    pub exchanges: &'a str,
    /// `--fail-on-undocumented`.
    pub fail_on_undocumented: bool,
}

/// A conformance check as a `mode: conformance` report: one item per
/// exchange (`violation`, `undocumented` or `conforms`) and the coverage.
pub fn conformance_report(
    result: &Conformance,
    spec: &crate::Spec,
    sources: ConformanceSources<'_>,
    redactor: &Redactor,
) -> ContractReport {
    let list = vec![
        source("spec", sources.spec),
        source("exchanges", sources.exchanges),
    ];
    let mut report = header(ContractMode::Conformance, list);
    report.items = result
        .checked
        .iter()
        .map(|c| exchange_item(c, spec, sources))
        .collect();
    report.summary = summary(&["violation", "undocumented", "conforms"], &report.items);
    report.coverage = Some(result.coverage.clone());
    redacted(report, redactor)
}

fn exchange_item(
    checked: &conformance::Checked,
    spec: &crate::Spec,
    sources: ConformanceSources<'_>,
) -> ContractItem {
    use conformance::Verdict;
    let (rule, description) = match checked.verdict {
        Verdict::Violation => (
            "conformance-violation",
            "A recorded exchange breaks the document",
        ),
        Verdict::Undocumented => (
            "undocumented-exchange",
            "No documented operation matches a recorded exchange",
        ),
        Verdict::Conforms => ("conforms", "A recorded exchange conforms to the document"),
    };
    let exchange = &checked.exchange;
    let detail = match checked.verdict {
        Verdict::Violation => checked.messages.join("; "),
        Verdict::Undocumented => "undocumented".to_owned(),
        Verdict::Conforms => "conforms".to_owned(),
    };
    let mut locations = Vec::new();
    if let Some(index) = checked.operation {
        locations.push(ContractLocation {
            role: "spec".to_owned(),
            document: sources.spec.to_owned(),
            pointer: spec.model.operations[index].pointer.clone(),
        });
    }
    locations.push(ContractLocation {
        role: "exchange".to_owned(),
        document: exchange.location.clone(),
        pointer: String::new(),
    });
    ContractItem {
        rule: rule.to_owned(),
        description: description.to_owned(),
        status: checked.verdict.as_str().to_owned(),
        allowed: None,
        failing: match checked.verdict {
            Verdict::Violation => true,
            Verdict::Undocumented => sources.fail_on_undocumented,
            Verdict::Conforms => false,
        },
        subject: checked.subject.clone(),
        message: format!(
            "exchange {} ({} {}): {detail}",
            exchange.id, exchange.method, exchange.url
        ),
        locations,
    }
}

/// `report` with every string redacted. Documents can hold anything (an
/// example with a real token, a server URL with credentials), so nothing
/// read from them reaches a report unredacted.
pub fn redacted(report: ContractReport, redactor: &Redactor) -> ContractReport {
    if redactor.is_empty() {
        return report;
    }
    let value = serde_json::to_value(&report).expect("contract reports serialize");
    let value = rustman_core::run::redact_value(&value, redactor);
    serde_json::from_value(value).expect("redaction keeps the report's shape")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::Applied;
    use crate::catalog::RuleId;
    use crate::difference::Diff;
    use crate::verdict::Strictness;
    use rustman_core::Secret;

    fn verdict() -> Verdict {
        let rule = RuleId::ResponseEnumWidened;
        let differences = vec![Difference {
            rule,
            classification: rule.classification(),
            operation: "GET /users".into(),
            message: "enum gains 'canary-token-123'".into(),
            old_pointer: "/a".into(),
            new_pointer: "/b".into(),
        }];
        let applied = Applied::default();
        Verdict::new(
            Diff::new("v1.yaml", "v2.yaml", differences),
            applied,
            Strictness::default(),
        )
    }

    #[test]
    fn a_diff_report_names_both_documents_and_counts() {
        let report = diff_report(&verdict(), &Redactor::new());
        assert_eq!(report.kind, "contract");
        assert_eq!(report.mode, ContractMode::Diff);
        assert_eq!(
            report.summary.line(),
            "0 breaking, 0 non-breaking, 1 unknown"
        );
        assert!(report.summary.passed);
        let item = &report.items[0];
        assert_eq!(item.locations[0].document, "v2.yaml");
        assert_eq!(item.locations[0].pointer, "/b");
        assert_eq!(item.locations[1].pointer, "/a");
    }

    #[test]
    fn secrets_in_documents_are_redacted() {
        let secret = Secret::new("token".to_owned(), "canary-token-123".to_owned());
        let report = diff_report(&verdict(), &Redactor::new().with(&secret));
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("canary-token-123"), "{json}");
        assert!(json.contains("«redacted:token»"), "{json}");
    }
}
