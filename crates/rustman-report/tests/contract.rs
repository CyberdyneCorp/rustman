//! Contract reports (add-contract-testing tasks 4.1 and 4.2): the
//! `kind: contract` report against the published schema, its round trip,
//! every format as a snapshot, and SARIF against the SARIF 2.1.0 schema.

use rustman_report::contract::{
    ContractCoverage, ContractItem, ContractLocation, ContractMode, ContractReport, ContractSource,
    ContractSummary, KIND_CONTRACT, OperationCoverage, StatusCount,
};
use rustman_report::{
    ReadError, RenderFormat, RenderOptions, Report, SCHEMA, render, report_schema,
};
use serde_json::Value;

fn location(role: &str, document: &str, pointer: &str) -> ContractLocation {
    ContractLocation {
        role: role.into(),
        document: document.into(),
        pointer: pointer.into(),
    }
}

fn item(
    rule: &str,
    status: &str,
    subject: &str,
    message: &str,
    pointers: [&str; 2],
) -> ContractItem {
    ContractItem {
        rule: rule.into(),
        description: format!("The {rule} rule."),
        status: status.into(),
        allowed: None,
        failing: status == "breaking",
        subject: subject.into(),
        message: message.into(),
        locations: vec![
            location("new", "specs/v2.yaml", pointers[1]),
            location("old", "origin/main:specs/v2.yaml", pointers[0]),
        ],
    }
}

fn count(status: &str, count: usize) -> StatusCount {
    StatusCount {
        status: status.into(),
        count,
    }
}

fn contract() -> ContractReport {
    let mut allowed = item(
        "operation-removed",
        "breaking",
        "DELETE /users/{id}",
        "operation removed",
        ["/paths/~1users~1{id}/delete", "/paths/~1users~1{id}"],
    );
    allowed.allowed = Some("Deprecated since 2026-06, no traffic".into());
    allowed.failing = false;
    ContractReport {
        schema: SCHEMA.into(),
        kind: KIND_CONTRACT.into(),
        rustman_version: "0.1.0".into(),
        mode: ContractMode::Diff,
        sources: vec![
            ContractSource {
                role: "old".into(),
                name: "origin/main:specs/v2.yaml".into(),
            },
            ContractSource {
                role: "new".into(),
                name: "specs/v2.yaml".into(),
            },
        ],
        catalog_version: Some(1),
        summary: ContractSummary {
            counts: vec![
                count("breaking", 2),
                count("non-breaking", 1),
                count("unknown", 1),
            ],
            allowed: 1,
            failing: 1,
            passed: false,
        },
        items: vec![
            item(
                "operation-added",
                "non-breaking",
                "GET /health",
                "operation added",
                ["/paths", "/paths/~1health/get"],
            ),
            allowed,
            item(
                "response-field-removed",
                "breaking",
                "GET /users/{id}",
                "response 200 (application/json) $.email: removed | <b>",
                [
                    "/components/schemas/User/properties/email",
                    "/components/schemas/User/properties",
                ],
            ),
            item(
                "composition-changed",
                "unknown",
                "GET /pets",
                "response 200 (application/json) $: oneOf changed",
                ["/components/schemas/Pet", "/components/schemas/Pet"],
            ),
        ],
        warnings: vec![
            "contracts/allowlist.yaml:7: unused allowlist entry (operation-removed GET /old)"
                .into(),
        ],
        coverage: None,
    }
}

fn report() -> Report {
    Report::Contract(Box::new(contract()))
}

fn rendered(format: RenderFormat) -> String {
    render(&report(), format, &RenderOptions::default())
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn a_contract_report_validates_and_round_trips() {
    let validator =
        jsonschema::validator_for(&serde_json::from_str(&report_schema()).unwrap()).unwrap();
    let text = report().to_json();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_valid(&validator, &value);
    assert_eq!(Report::parse(&text).unwrap(), report());
    assert_eq!(report().kind(), "contract");
    let mut broken = value.clone();
    broken["mode"] = Value::from("bench");
    assert!(!validator.is_valid(&broken));
}

#[test]
fn a_kind_this_reader_does_not_know_is_refused() {
    let mut value: Value = serde_json::from_str(&report().to_json()).unwrap();
    value["kind"] = Value::from("contract-next");
    let err = Report::parse(&value.to_string()).unwrap_err();
    assert!(matches!(err, ReadError::UnknownKind(ref k) if k == "contract-next"));
    assert_eq!(
        err.to_string(),
        "unknown report kind 'contract-next' (expected run, exploration or contract)"
    );
}

#[test]
fn snapshot_contract_formats() {
    for format in RenderFormat::ALL {
        insta::assert_snapshot!(format!("contract_{format}"), rendered(format));
    }
}

#[test]
fn markdown_has_the_counts_then_breaking_differences_first() {
    let markdown = rendered(RenderFormat::Markdown);
    assert!(markdown.contains(
        "**Summary:** 2 breaking, 1 non-breaking, 1 unknown, 1 allowed. Failed: 1 failing."
    ));
    let breaking = markdown.find("## Breaking (2)").unwrap();
    let other = markdown.find("## Non-breaking (1)").unwrap();
    assert!(breaking < other);
    let failing = markdown.find("`response-field-removed` (failing)").unwrap();
    let allowed = markdown.find("`operation-removed` (allowed)").unwrap();
    assert!(breaking < failing && failing < allowed && allowed < other);
    assert!(
        markdown.contains("\\| \\<b\\>"),
        "untrusted text is escaped"
    );
}

#[test]
fn contract_sarif_validates_against_the_sarif_2_1_0_schema() {
    let path = format!(
        "{}/tests/fixtures/sarif-schema-2.1.0.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .build(&schema)
        .unwrap();
    let sarif: Value = serde_json::from_str(&rendered(RenderFormat::Sarif)).unwrap();
    assert_valid(&validator, &sarif);
}

#[test]
fn a_breaking_difference_is_an_error_at_the_new_document_pointer() {
    let sarif: Value = serde_json::from_str(&rendered(RenderFormat::Sarif)).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    let levels: Vec<&str> = results
        .iter()
        .map(|r| r["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, ["note", "note", "error", "warning"]);
    let breaking = &results[2];
    let location = &breaking["locations"][0];
    assert_eq!(
        location["physicalLocation"]["artifactLocation"]["uri"],
        "specs/v2.yaml"
    );
    assert_eq!(
        location["logicalLocations"][0]["fullyQualifiedName"],
        "/components/schemas/User/properties"
    );
    assert_eq!(
        results[1]["suppressions"][0]["justification"],
        "Deprecated since 2026-06, no traffic"
    );
    let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap();
    assert_eq!(rules.len(), 4, "one reportingDescriptor per rule id");
}

#[test]
fn junit_fails_only_the_failing_items() {
    let junit = rendered(RenderFormat::Junit);
    let doc = roxmltree::Document::parse(&junit).unwrap();
    let failures: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("failure"))
        .map(|n| n.attribute("type").unwrap())
        .collect();
    assert_eq!(failures, ["response-field-removed"]);
    let cases = doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .count();
    assert_eq!(cases, 4);
    assert!(junit.contains("allowed: Deprecated since 2026-06, no traffic"));
}

/// A conformance report carries its coverage (add-contract-testing task
/// 7.1): it validates, round trips and shows in Markdown.
#[test]
fn conformance_coverage_validates_and_renders() {
    let mut conformance = contract();
    conformance.mode = ContractMode::Conformance;
    conformance.coverage = Some(ContractCoverage {
        observed: 1,
        total: 2,
        operations: vec![
            OperationCoverage {
                operation: "GET /users/{id}".into(),
                exchanges: 3,
                violations: 1,
            },
            OperationCoverage {
                operation: "DELETE /users/{id}".into(),
                exchanges: 0,
                violations: 0,
            },
        ],
    });
    let report = Report::Contract(Box::new(conformance));
    let validator =
        jsonschema::validator_for(&serde_json::from_str(&report_schema()).unwrap()).unwrap();
    let text = report.to_json();
    assert_valid(&validator, &serde_json::from_str(&text).unwrap());
    assert_eq!(Report::parse(&text).unwrap(), report);
    let markdown = render(&report, RenderFormat::Markdown, &RenderOptions::default());
    assert!(
        markdown.contains("failing. 1/2 operations observed."),
        "{markdown}"
    );
    assert!(
        markdown.contains("## Coverage\n\n| Operation | Exchanges | Violations |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| `GET /users/{id}` | 3 | 1 |"),
        "{markdown}"
    );
}
