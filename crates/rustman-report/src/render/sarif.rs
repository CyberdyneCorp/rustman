//! SARIF 2.1.0, for code-scanning dashboards. Each rule a finding broke is
//! a `reportingDescriptor`; each finding is a `result` whose level maps
//! `critical`/`high` to `error`, `medium` to `warning` and `low`/`info` to
//! `note`, located in the OpenAPI document at the operation's JSON pointer.
//! A run's failed and errored requests are results located in their
//! request files. A contract check's items are results too, one
//! `reportingDescriptor` per rule id: an item that fails the command is an
//! `error`, located in its primary document (for a diff, the new one) at
//! the JSON pointer of the changed element; an allowlisted one carries the
//! justification as an external suppression.

use std::collections::BTreeMap;

use serde::Serialize;

use super::RenderOptions;
use super::view::{line_of, operations};
use crate::bench::BenchReport;
use crate::contract::{ContractItem, ContractReport};
use crate::exploration::{ExplorationReport, ReportFinding, Severity};
use crate::run::{RunReport, RunRequest, Status};
use crate::stress::StressReport;

const SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const VERSION: &str = "2.1.0";
const TOOL: &str = "rustman";
const INFORMATION_URI: &str = "https://github.com/CyberdyneCorp/rustman";

#[derive(Serialize)]
struct Log {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    automation_details: Option<Automation>,
}

#[derive(Serialize)]
struct Automation {
    id: String,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: &'static str,
    version: String,
    information_uri: &'static str,
    rules: Vec<Descriptor>,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

fn text(s: impl Into<String>) -> Text {
    Text { text: s.into() }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    id: String,
    short_description: Text,
    full_description: Text,
    help: Text,
    default_configuration: Configuration,
    properties: DescriptorProperties,
}

#[derive(Serialize)]
struct Configuration {
    level: &'static str,
}

#[derive(Serialize)]
struct DescriptorProperties {
    tags: Vec<&'static str>,
    #[serde(rename = "security-severity", skip_serializing_if = "Option::is_none")]
    security_severity: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: String,
    rule_index: usize,
    level: &'static str,
    message: Text,
    locations: Vec<Location>,
    partial_fingerprints: BTreeMap<&'static str, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    suppressions: Vec<Suppression>,
    properties: BTreeMap<&'static str, String>,
}

#[derive(Serialize)]
struct Suppression {
    kind: &'static str,
    justification: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    physical_location: Physical,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    logical_locations: Vec<Logical>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Physical {
    artifact_location: ArtifactLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<Region>,
}

#[derive(Serialize)]
struct ArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Logical {
    name: String,
    fully_qualified_name: String,
    kind: &'static str,
}

/// The SARIF level of a severity.
pub fn level(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical | Severity::High => "error",
        Severity::Medium => "warning",
        Severity::Low | Severity::Info => "note",
    }
}

/// GitHub code scanning's `security-severity` score of a severity.
fn score(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "9.5",
        Severity::High => "8.0",
        Severity::Medium => "5.5",
        Severity::Low => "3.0",
        Severity::Info => "0.0",
    }
}

/// An exploration as a SARIF log.
pub fn exploration_sarif(report: &ExplorationReport, options: &RenderOptions) -> String {
    let uri = options.document_uri(&report.spec);
    let views = operations(report);
    let mut rules: Vec<Descriptor> = Vec::new();
    let mut results = Vec::new();
    for finding in &report.findings {
        let index = rule_index(&mut rules, finding);
        let pointer = views
            .iter()
            .find(|v| v.operation == finding.operation)
            .map_or_else(String::new, |v| v.pointer.clone());
        let region = options
            .document
            .as_deref()
            .and_then(|d| line_of(d, &finding.operation))
            .map(|start_line| Region { start_line });
        results.push(finding_result(finding, index, &uri, pointer, region));
    }
    to_text(&Log {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: tool(rules),
            results,
            automation_details: Some(Automation {
                id: format!("rustman/explore/{}", report.id),
            }),
        }],
    })
}

fn rule_index(rules: &mut Vec<Descriptor>, finding: &ReportFinding) -> usize {
    if let Some(i) = rules.iter().position(|r| r.id == finding.rule) {
        return i;
    }
    let help = if finding.remediation.is_empty() {
        finding.description.clone()
    } else {
        finding.remediation.clone()
    };
    rules.push(Descriptor {
        id: finding.rule.clone(),
        short_description: text(finding.rule.clone()),
        full_description: text(finding.description.clone()),
        help: text(help),
        default_configuration: Configuration {
            level: level(finding.severity),
        },
        properties: DescriptorProperties {
            tags: vec!["security", "api"],
            security_severity: Some(score(finding.severity)),
        },
    });
    rules.len() - 1
}

fn finding_result(
    finding: &ReportFinding,
    rule_index: usize,
    uri: &str,
    pointer: String,
    region: Option<Region>,
) -> SarifResult {
    let message = format!("{}: {}", finding.operation, finding.message);
    let fingerprint = format!("{}|{}", finding.rule, finding.operation);
    SarifResult {
        rule_id: finding.rule.clone(),
        rule_index,
        level: level(finding.severity),
        message: text(message),
        locations: vec![Location {
            physical_location: Physical {
                artifact_location: ArtifactLocation {
                    uri: uri.to_owned(),
                },
                region,
            },
            logical_locations: vec![Logical {
                name: finding.operation.clone(),
                fully_qualified_name: pointer.clone(),
                kind: "object",
            }],
        }],
        partial_fingerprints: BTreeMap::from([("rustmanFinding/v1", fingerprint)]),
        suppressions: Vec::new(),
        properties: BTreeMap::from([
            ("finding", finding.id.clone()),
            ("severity", finding.severity.to_string()),
            ("operation", finding.operation.clone()),
            ("pointer", pointer),
            ("occurrences", finding.occurrences.to_string()),
        ]),
    }
}

/// A run as a SARIF log: one result per failed or errored request.
pub fn run_sarif(run: &RunReport) -> String {
    let rules = vec![
        run_rule(
            "assertion-failed",
            "A request's assertions did not pass.",
            Severity::High,
        ),
        run_rule(
            "request-errored",
            "A request got no response: network, TLS, timeout or unresolved variables.",
            Severity::High,
        ),
    ];
    let results = run.requests.iter().filter_map(request_result).collect();
    to_text(&Log {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: tool(rules),
            results,
            automation_details: None,
        }],
    })
}

fn run_rule(id: &str, description: &str, severity: Severity) -> Descriptor {
    Descriptor {
        id: id.to_owned(),
        short_description: text(id),
        full_description: text(description),
        help: text(description),
        default_configuration: Configuration {
            level: level(severity),
        },
        properties: DescriptorProperties {
            tags: vec!["api"],
            security_severity: Some(score(severity)),
        },
    }
}

fn request_result(request: &RunRequest) -> Option<SarifResult> {
    let (rule_id, rule_index) = match request.status {
        Status::Failed => ("assertion-failed", 0),
        Status::Errored => ("request-errored", 1),
        Status::Passed | Status::Skipped => return None,
    };
    Some(SarifResult {
        rule_id: rule_id.to_owned(),
        rule_index,
        level: "error",
        message: text(format!("{}: {}", request.id, request.reasons().join("; "))),
        locations: vec![Location {
            physical_location: Physical {
                artifact_location: ArtifactLocation {
                    uri: request.file.clone(),
                },
                region: None,
            },
            logical_locations: Vec::new(),
        }],
        partial_fingerprints: BTreeMap::from([(
            "rustmanRequest/v1",
            format!("{rule_id}|{}", request.id),
        )]),
        suppressions: Vec::new(),
        properties: BTreeMap::from([("iteration", request.iteration.to_string())]),
    })
}

/// A bench as a SARIF log: one `error` result per breached threshold,
/// without a location (a threshold is about the whole run).
pub fn bench_sarif(report: &BenchReport) -> String {
    const RULE: &str = "bench-threshold";
    let description = "A load-test threshold was breached.";
    let rule = Descriptor {
        id: RULE.to_owned(),
        short_description: text(RULE),
        full_description: text(description),
        help: text(description),
        default_configuration: Configuration { level: "error" },
        properties: DescriptorProperties {
            tags: vec!["api", "performance"],
            security_severity: None,
        },
    };
    let results = report
        .thresholds
        .iter()
        .filter(|t| !t.passed)
        .map(|t| SarifResult {
            rule_id: RULE.to_owned(),
            rule_index: 0,
            level: "error",
            message: text(t.line()),
            locations: Vec::new(),
            partial_fingerprints: BTreeMap::from([(
                "rustmanThreshold/v1",
                format!("{}|{}", report.target, t.expression),
            )]),
            suppressions: Vec::new(),
            properties: BTreeMap::from([("actual", t.actual.clone())]),
        })
        .collect();
    to_text(&Log {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: tool(vec![rule]),
            results,
            automation_details: Some(Automation {
                id: "rustman/bench".to_owned(),
            }),
        }],
    })
}

/// A stress run as a SARIF log: one result per finding, located in the
/// request file it concerns.
pub fn stress_sarif(report: &StressReport) -> String {
    let mut rules: Vec<Descriptor> = Vec::new();
    let results = report
        .findings
        .iter()
        .map(|f| {
            let index = rule_index(&mut rules, &f.finding);
            finding_result(&f.finding, index, &f.finding.operation, String::new(), None)
        })
        .collect();
    to_text(&Log {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: tool(rules),
            results,
            automation_details: Some(Automation {
                id: format!("rustman/stress/{}", report.id),
            }),
        }],
    })
}

/// A contract check as a SARIF log: one result per item.
pub fn contract_sarif(report: &ContractReport, options: &RenderOptions) -> String {
    let mut rules: Vec<Descriptor> = Vec::new();
    let results = report
        .items
        .iter()
        .map(|item| {
            let index = contract_rule_index(&mut rules, item);
            contract_result(item, index, options)
        })
        .collect();
    to_text(&Log {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: tool(rules),
            results,
            automation_details: Some(Automation {
                id: format!("rustman/contract/{}", report.mode),
            }),
        }],
    })
}

/// `error` for an item that fails the command, `warning` for an
/// unallowed `unknown`, `note` otherwise.
fn contract_level(item: &ContractItem) -> &'static str {
    if item.failing {
        "error"
    } else if item.allowed.is_none() && item.status == "unknown" {
        "warning"
    } else {
        "note"
    }
}

fn contract_rule_index(rules: &mut Vec<Descriptor>, item: &ContractItem) -> usize {
    if let Some(i) = rules.iter().position(|r| r.id == item.rule) {
        return i;
    }
    let level = match item.status.as_str() {
        "breaking" | "failed" | "violation" | "blocked" => "error",
        "unknown" | "undocumented" => "warning",
        _ => "note",
    };
    rules.push(Descriptor {
        id: item.rule.clone(),
        short_description: text(item.rule.clone()),
        full_description: text(item.description.clone()),
        help: text(item.description.clone()),
        default_configuration: Configuration { level },
        properties: DescriptorProperties {
            tags: vec!["api", "contract"],
            security_severity: None,
        },
    });
    rules.len() - 1
}

fn contract_result(item: &ContractItem, rule_index: usize, options: &RenderOptions) -> SarifResult {
    let primary = item.locations.first();
    let pointer = primary.map_or_else(String::new, |l| l.pointer.clone());
    let locations = primary
        .map(|l| Location {
            physical_location: Physical {
                artifact_location: ArtifactLocation {
                    uri: options.document_uri(&l.document),
                },
                region: None,
            },
            logical_locations: vec![Logical {
                name: item.subject.clone(),
                fully_qualified_name: l.pointer.clone(),
                kind: "object",
            }],
        })
        .into_iter()
        .collect();
    let suppressions = item
        .allowed
        .iter()
        .map(|why| Suppression {
            kind: "external",
            justification: why.clone(),
        })
        .collect();
    SarifResult {
        rule_id: item.rule.clone(),
        rule_index,
        level: contract_level(item),
        message: text(format!("{}: {}", item.subject, item.message)),
        locations,
        partial_fingerprints: BTreeMap::from([(
            "rustmanContract/v1",
            format!("{}|{}|{pointer}", item.rule, item.subject),
        )]),
        suppressions,
        properties: BTreeMap::from([
            ("status", item.status.clone()),
            ("operation", item.subject.clone()),
            ("pointer", pointer),
        ]),
    }
}

fn tool(rules: Vec<Descriptor>) -> Tool {
    Tool {
        driver: Driver {
            name: TOOL,
            version: rustman_core::VERSION.to_owned(),
            information_uri: INFORMATION_URI,
            rules,
        },
    }
}

fn to_text(log: &Log) -> String {
    let mut text = serde_json::to_string_pretty(log).expect("SARIF serializes");
    text.push('\n');
    text
}
