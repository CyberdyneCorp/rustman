//! The canonical report and its renderers (add-openapi-explorer tasks
//! 4.1 to 4.6): the generated schema against the published file, every
//! format of an exploration and a run as snapshots, SARIF against the
//! SARIF 2.1.0 schema, the JUnit mapping, the offline HTML, `report diff`
//! on report pairs, and redaction across all five formats.

use rustman_report::exploration::{
    Cell, Cleanup, Coverage, Evidence, ExplorationReport, Family, OperationInfo, ProbeResult,
    ProbeTotals, Ratio, ReportFinding, Reproduction, Resource, Row, Settings, Severity,
    SeverityCounts, StatusCell, Summary,
};
use rustman_report::{RenderFormat, RenderOptions, Report, SCHEMA, diff, render, report_schema};
use serde_json::Value;

/// The secret the fixtures were built with. Every text in a report is
/// redacted when it is built, so it only ever appears as its placeholder.
const SECRET: &str = "s3cr3t";
const PLACEHOLDER: &str = "«redacted:bearerAuth_token»";

fn settings() -> Settings {
    Settings {
        probes: Family::defaults().into_iter().collect(),
        operations: Vec::new(),
        seed: 7,
        fuzz_cases: 4,
        allow_mutations: false,
        allow_destructive: false,
        allow_hosts: Vec::new(),
        production: None,
        allow_remote_refs: false,
        rate: 10,
        concurrency: 4,
        max_probes: 2000,
        max_duration_ms: 900_000,
        fail_on: Severity::High,
    }
}

fn evidence(method: &str, path: &str, status: u16) -> Evidence {
    Evidence {
        method: method.into(),
        url: format!("http://127.0.0.1:8080{path}"),
        request_headers: vec![("Authorization".into(), format!("Bearer {PLACEHOLDER}"))],
        request_body: (method == "POST").then(|| "{\"age\": \"x\"} <b>".into()),
        status,
        response_headers: vec![("content-type".into(), "text/plain".into())],
        body: format!("echo: authorization: Bearer {PLACEHOLDER}\n<script>alert(1)</script>"),
        truncated: false,
    }
}

fn finding(
    id: &str,
    rule: &str,
    severity: Severity,
    operation: &str,
    status: u16,
) -> ReportFinding {
    let (method, path) = operation.split_once(' ').unwrap();
    ReportFinding {
        id: id.into(),
        rule: rule.into(),
        severity,
        operation: operation.into(),
        description: format!("The {rule} rule."),
        remediation: format!("Fix {rule}."),
        message: format!("{rule} on {operation} | got {status}"),
        occurrences: 2,
        probes: vec!["p0001".into()],
        instance_pointer: None,
        schema_pointer: None,
        evidence: evidence(method, path, status),
        reproduction: Some(Reproduction {
            curl: format!(
                "curl 'http://127.0.0.1:8080{path}' -H 'Authorization: Bearer {{{{bearerAuth_token}}}}'"
            ),
            request: String::new(),
        }),
    }
}

fn probe(id: &str, operation: &str, outcome: &str) -> ProbeResult {
    ProbeResult {
        id: id.into(),
        family: Family::Conformance,
        operation: operation.into(),
        method: operation.split(' ').next().unwrap().into(),
        variant: "documented".into(),
        outcome: outcome.into(),
        reason: (outcome == "blocked").then(|| "DELETE needs --allow-destructive".into()),
    }
}

fn operation(op: &str, tag: Option<&str>) -> OperationInfo {
    OperationInfo {
        operation: op.into(),
        operation_id: None,
        tags: tag.map(|t| vec![t.to_owned()]).unwrap_or_default(),
    }
}

/// A read-only exploration of four operations: a critical and a high
/// finding, a medium one, an undocumented method, and a blocked DELETE.
fn exploration() -> ExplorationReport {
    let findings = vec![
        finding(
            "f001",
            "auth-not-enforced",
            Severity::Critical,
            "GET /admin/users",
            200,
        ),
        finding(
            "f002",
            "server-error-on-invalid-input",
            Severity::High,
            "POST /users",
            500,
        ),
        finding(
            "f003",
            "response-schema-violation",
            Severity::Medium,
            "GET /users/{id}",
            200,
        ),
        finding(
            "f004",
            "undocumented-method-allowed",
            Severity::Medium,
            "PUT /users",
            200,
        ),
        finding(
            "f005",
            "missing-content-type-options",
            Severity::Low,
            "POST /users",
            500,
        ),
    ];
    let mut counts = SeverityCounts::default();
    findings.iter().for_each(|f| counts.add(f.severity));
    let probes = vec![
        probe("p0001", "GET /admin/users", "finding"),
        probe("p0002", "POST /users", "finding"),
        probe("p0003", "GET /users/{id}", "finding"),
        probe("p0004", "DELETE /users/{id}", "blocked"),
        probe("p0005", "DELETE /users/{id}", "blocked"),
    ];
    let row = |op: &str, statuses: &[(&str, Cell)]| Row {
        operation: op.into(),
        statuses: statuses
            .iter()
            .map(|(s, state)| StatusCell {
                status: (*s).into(),
                state: *state,
            })
            .collect(),
    };
    ExplorationReport {
        schema: SCHEMA.into(),
        kind: "exploration".into(),
        rustman_version: "0.1.0".into(),
        id: "exp-20261118-0a1b2c".into(),
        started_at: "2026-11-18T09:00:00.000Z".into(),
        finished_at: "2026-11-18T09:00:01.500Z".into(),
        environment: Some("dev".into()),
        target: "http://127.0.0.1:8080".into(),
        spec: "api.yaml".into(),
        document_sha256: "ab".repeat(32),
        seed: 7,
        settings: settings(),
        summary: Summary {
            probes: ProbeTotals {
                total: 5,
                finding: 3,
                blocked: 2,
                ..ProbeTotals::default()
            },
            findings: counts,
            requests: 3,
            duration_ms: 1500,
        },
        coverage: Coverage {
            operations: Ratio::new(3, 4),
            statuses: Ratio::new(3, 6),
            matrix: vec![
                row(
                    "GET /admin/users",
                    &[("200", Cell::Observed), ("401", Cell::NotObserved)],
                ),
                row("POST /users", &[("201", Cell::NotObserved)]),
                row(
                    "GET /users/{id}",
                    &[("200", Cell::Observed), ("404", Cell::Observed)],
                ),
                row("DELETE /users/{id}", &[("204", Cell::Blocked)]),
            ],
        },
        findings,
        probes,
        left_behind: vec![Resource {
            probe: "p0002".into(),
            operation: "POST /users".into(),
            path: "/users/9".into(),
            url: "http://127.0.0.1:8080/users/9".into(),
            delete: Some("DELETE /users/{id}".into()),
        }],
        cleanup: vec![Cleanup {
            path: "/users/8".into(),
            url: "http://127.0.0.1:8080/users/8".into(),
            status: Some(204),
            error: None,
        }],
        operations: vec![
            operation("GET /admin/users", Some("admin")),
            operation("POST /users", Some("users")),
            operation("GET /users/{id}", Some("users")),
            operation("DELETE /users/{id}", Some("users")),
        ],
        dependencies: Vec::new(),
    }
}

/// The published run report of the `bootstrap-core-engine` reporters.
fn run_report() -> Report {
    Report::parse(include_str!("fixtures/run-v1.json")).unwrap()
}

const DOCUMENT: &str = "openapi: 3.1.0\npaths:\n  /admin/users:\n    get:\n      x: 1\n  /users:\n    post:\n      x: 1\n  /users/{id}:\n    get:\n      x: 1\n    delete:\n      x: 1\n";

fn options() -> RenderOptions {
    RenderOptions {
        document: Some(DOCUMENT.into()),
        ..RenderOptions::default()
    }
}

fn rendered(report: &Report, format: RenderFormat) -> String {
    render(report, format, &options())
}

fn json_file(path: &str) -> Value {
    let path = format!("{}/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const PUBLISHED: &str = "../../docs/schemas/report.schema.json";

#[test]
fn the_generated_schema_equals_the_published_file() {
    let path = format!("{}/{PUBLISHED}", env!("CARGO_MANIFEST_DIR"));
    let generated = report_schema();
    if std::env::var_os("RUSTMAN_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    let published = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        published == generated,
        "docs/schemas/report.schema.json is stale; regenerate it with RUSTMAN_UPDATE_SCHEMA=1 cargo test -p rustman-report --test render"
    );
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&serde_json::from_str(&report_schema()).unwrap()).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, report: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn reports_of_both_kinds_validate_and_round_trip() {
    let validator = validator();
    let exploration = Report::Exploration(Box::new(exploration()));
    let text = exploration.to_json();
    assert_valid(&validator, &serde_json::from_str(&text).unwrap());
    assert_eq!(Report::parse(&text).unwrap(), exploration);
    // A run report written before this change still validates and reads.
    assert_valid(&validator, &json_file("tests/fixtures/run-v1.json"));
    assert_eq!(
        run_report().to_json(),
        include_str!("fixtures/run-v1.json").replace("\r\n", "\n")
    );
    // An exploration report is not a run report, and the other way round.
    let mut broken: Value = serde_json::from_str(&text).unwrap();
    broken.as_object_mut().unwrap().remove("findings");
    assert!(!validator.is_valid(&broken));
}

#[test]
fn snapshot_exploration_formats() {
    let report = Report::Exploration(Box::new(exploration()));
    for format in RenderFormat::ALL {
        insta::assert_snapshot!(format!("exploration_{format}"), rendered(&report, format));
    }
}

#[test]
fn snapshot_run_formats() {
    let report = run_report();
    for format in [
        RenderFormat::Markdown,
        RenderFormat::Html,
        RenderFormat::Sarif,
    ] {
        insta::assert_snapshot!(format!("run_{format}"), rendered(&report, format));
    }
}

#[test]
fn sarif_validates_against_the_sarif_2_1_0_schema() {
    let schema = json_file("tests/fixtures/sarif-schema-2.1.0.json");
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .build(&schema)
        .unwrap();
    for report in [Report::Exploration(Box::new(exploration())), run_report()] {
        let sarif: Value = serde_json::from_str(&rendered(&report, RenderFormat::Sarif)).unwrap();
        assert_valid(&validator, &sarif);
        assert_eq!(sarif["version"], "2.1.0");
    }
}

#[test]
fn sarif_levels_and_locations_follow_the_finding() {
    let report = Report::Exploration(Box::new(exploration()));
    let sarif: Value = serde_json::from_str(&rendered(&report, RenderFormat::Sarif)).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    let levels: Vec<&str> = results
        .iter()
        .map(|r| r["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, ["error", "error", "warning", "warning", "note"]);
    let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap();
    assert_eq!(rules.len(), 5);
    assert_eq!(rules[0]["id"], "auth-not-enforced");
    // A finding on GET /users/{id} from api.yaml.
    let location = &results[2]["locations"][0];
    assert_eq!(
        location["physicalLocation"]["artifactLocation"]["uri"],
        "api.yaml"
    );
    assert_eq!(location["physicalLocation"]["region"]["startLine"], 10);
    assert_eq!(
        location["logicalLocations"][0]["fullyQualifiedName"],
        "/paths/~1users~1{id}/get"
    );
    // An undocumented method points at its path item.
    assert_eq!(
        results[3]["locations"][0]["logicalLocations"][0]["fullyQualifiedName"],
        "/paths/~1users"
    );
}

#[test]
fn junit_fails_operations_at_or_above_fail_on_and_skips_blocked_ones() {
    let report = Report::Exploration(Box::new(exploration()));
    let xml = rendered(&report, RenderFormat::Junit);
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let case = |name: &str| {
        doc.descendants()
            .find(|n| n.has_tag_name("testcase") && n.attribute("name") == Some(name))
            .unwrap_or_else(|| panic!("no testcase {name}"))
    };
    let child = |name: &str, tag: &str| case(name).children().find(|n| n.has_tag_name(tag));
    let failure = child("POST /users", "failure").expect("POST /users fails");
    assert_eq!(
        failure.attribute("type"),
        Some("server-error-on-invalid-input")
    );
    assert!(
        failure
            .attribute("message")
            .unwrap()
            .contains("server-error-on-invalid-input")
    );
    assert!(
        child("GET /users/{id}", "failure").is_none(),
        "medium is below high"
    );
    assert!(child("DELETE /users/{id}", "skipped").is_some());
    let suites: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("testsuite"))
        .map(|n| n.attribute("name").unwrap())
        .collect();
    assert_eq!(suites, ["admin", "undocumented", "users"]);
    // A lower --fail-on fails the medium finding too.
    let medium = RenderOptions {
        fail_on: Some(Severity::Medium),
        ..RenderOptions::default()
    };
    let xml = render(&report, RenderFormat::Junit, &medium);
    assert_eq!(xml.matches("<failure").count(), 4);
}

#[test]
fn html_is_self_contained_and_escapes_evidence() {
    let html = rendered(
        &Report::Exploration(Box::new(exploration())),
        RenderFormat::Html,
    );
    for reference in [
        "src=\"http",
        "href=\"http",
        "url(http",
        "@import",
        "src='http",
        "href='http",
    ] {
        assert!(!html.contains(reference), "{reference}");
    }
    let lower = html.to_ascii_lowercase();
    for tag in ["<link", "<img", "<iframe", "<script src"] {
        assert!(!lower.contains(tag), "{tag}");
    }
    assert!(html.contains("<style>") && html.contains("<script>"));
    assert!(!html.contains("<script>alert(1)"), "evidence is escaped");
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;")
            || html.contains("&lt;script&gt;alert(1)&lt;/script&gt;")
    );
    // Evidence expands without JavaScript; the filters need it and start hidden.
    assert!(html.contains("<details class=\"finding\" data-severity=\"critical\""));
    assert!(html.contains("<div class=\"filters\" id=\"filters\" hidden>"));
}

#[test]
fn markdown_sorts_findings_by_severity_and_escapes_cells() {
    let md = rendered(
        &Report::Exploration(Box::new(exploration())),
        RenderFormat::Markdown,
    );
    let order: Vec<usize> = [
        "| f001 | critical",
        "| f002 | high",
        "| f003 | medium",
        "| f005 | low",
    ]
    .iter()
    .map(|row| md.find(row).unwrap_or_else(|| panic!("{row}")))
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert!(md.contains("POST /users \\| got 500"));
    assert!(md.contains("## Coverage"));
    assert!(md.contains("Operations exercised: 3/4 (75.0%)"));
}

#[test]
fn markdown_evidence_cannot_close_its_fence() {
    let mut report = exploration();
    report.findings[0].evidence.body =
        "ok\n~~~~~~\n# Injected heading\n<img src=x onerror=alert(1)>\n~~~~".into();
    let md = rendered(
        &Report::Exploration(Box::new(report)),
        RenderFormat::Markdown,
    );
    let at = md.find("# Injected heading").unwrap();
    // The block holding the body opens with seven tildes, longer than the
    // body's six, and only a line of seven closes it.
    let opened = md[..at].rfind("~~~~~~~http\n").expect("a longer fence");
    let closed = md[opened..].find("\n~~~~~~~\n").map(|i| opened + i);
    assert!(closed.is_some_and(|c| c > at), "{md}");
}

#[test]
fn markdown_messages_and_urls_cannot_inject_markup() {
    let mut report = exploration();
    report.findings[0].message = "at /name: <img src=x onerror=alert(1)> [x](javascript:y)".into();
    report.findings[0].evidence.url = "http://127.0.0.1:8080/a`<b>`".into();
    let md = rendered(
        &Report::Exploration(Box::new(report)),
        RenderFormat::Markdown,
    );
    assert!(!md.replace("\\<", "").contains("<img"), "{md}");
    assert!(md.contains("\\<img src=x onerror=alert\\(1\\)\\>"), "{md}");
    assert!(md.contains("\\[x\\]\\(javascript:y\\)"), "{md}");
    assert!(
        md.contains("Evidence: `` GET http://127.0.0.1:8080/a`<b>` `` answered"),
        "{md}"
    );
}

#[test]
fn no_format_contains_the_secret() {
    // The fixtures hold the secret only as its placeholder, as a report
    // built from redacted exchanges does. Rendering never resolves it back:
    // put the value anywhere in the report data and this test fails.
    for report in [Report::Exploration(Box::new(exploration())), run_report()] {
        for format in RenderFormat::ALL {
            let text = rendered(&report, format);
            assert!(!text.contains(SECRET), "{format} leaks the secret");
        }
    }
    let report = Report::Exploration(Box::new(exploration()));
    for format in [
        RenderFormat::Json,
        RenderFormat::Markdown,
        RenderFormat::Html,
    ] {
        assert!(rendered(&report, format).contains(PLACEHOLDER), "{format}");
    }
}

fn with_findings(keep: &[&str], extra: Option<ReportFinding>) -> Report {
    let mut r = exploration();
    r.findings.retain(|f| keep.contains(&f.id.as_str()));
    r.findings.extend(extra);
    Report::Exploration(Box::new(r))
}

#[test]
fn diff_lists_new_fixed_and_unchanged_findings() {
    let old = with_findings(&["f001", "f002", "f003"], None);
    let critical = finding(
        "f009",
        "broken-object-level-authorization",
        Severity::Critical,
        "GET /orders/{id}",
        200,
    );
    let mut new = with_findings(&["f001", "f003"], Some(critical));
    if let Report::Exploration(r) = &mut new {
        r.coverage.operations = Ratio::new(4, 4);
    }
    let d = diff(&old, &new).unwrap();
    let rules = |list: &[rustman_report::diff::DiffFinding]| -> Vec<String> {
        list.iter().map(|f| f.rule.clone()).collect()
    };
    assert_eq!(rules(&d.new), ["broken-object-level-authorization"]);
    assert_eq!(rules(&d.fixed), ["server-error-on-invalid-input"]);
    assert_eq!(
        rules(&d.unchanged),
        ["auth-not-enforced", "response-schema-violation"]
    );
    assert_eq!(d.regressions(Severity::High), 1);
    assert_eq!(d.coverage.unwrap().operations.delta, 25.0);
    // Fixed only: nothing regressed.
    let fixed = diff(&old, &with_findings(&["f001"], None)).unwrap();
    assert_eq!(fixed.regressions(Severity::Info), 0);
    assert_eq!(fixed.fixed.len(), 2);
    insta::assert_snapshot!("diff_markdown", d.to_markdown());
    let json: Value = serde_json::from_str(&d.to_json()).unwrap();
    assert_eq!(json["new"][0]["severity"], "critical");
    assert!(diff(&old, &run_report()).is_err());
}

#[test]
fn diff_lists_assertions_that_changed_state() {
    let old = run_report();
    let mut new = old.clone();
    if let Report::Run(r) = &mut new {
        let failed = r
            .requests
            .iter_mut()
            .find(|q| q.id == "users/create")
            .unwrap();
        failed.assertions[0].outcome = "pass".into();
    }
    let d = diff(&old, &new).unwrap();
    assert_eq!(d.assertions.len(), 1);
    let change = &d.assertions[0];
    assert_eq!(
        (
            change.request.as_str(),
            change.old.as_str(),
            change.new.as_str()
        ),
        ("users/create", "fail", "pass")
    );
    assert_eq!(d.regressions(Severity::Info), 0);
}

#[test]
fn diff_of_the_fixture_pair() {
    let read = |name: &str| {
        let path = format!("{}/tests/fixtures/diff/{name}", env!("CARGO_MANIFEST_DIR"));
        Report::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let validator = validator();
    for name in ["old.json", "new.json"] {
        assert_valid(
            &validator,
            &json_file(&format!("tests/fixtures/diff/{name}")),
        );
    }
    let d = diff(&read("old.json"), &read("new.json")).unwrap();
    assert_eq!(d.new.len(), 1);
    assert_eq!(
        (d.new[0].rule.as_str(), d.new[0].severity),
        ("broken-object-level-authorization", Severity::Critical)
    );
    assert_eq!(d.fixed[0].rule, "server-error-on-invalid-input");
    assert_eq!(d.unchanged.len(), 4);
    assert_eq!(d.coverage.unwrap().operations.delta, 25.0);
    assert_eq!(d.regressions(Severity::Critical), 1);
    // The other way round, the critical finding is fixed and nothing is new
    // at or above high.
    let back = diff(&read("new.json"), &read("old.json")).unwrap();
    assert_eq!(
        back.regressions(Severity::High),
        1,
        "the high one comes back"
    );
    assert_eq!(back.fixed[0].rule, "broken-object-level-authorization");
}
