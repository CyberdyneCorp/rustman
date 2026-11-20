//! Stress reports (add-stress-testing tasks 6.1, 6.2 and 6.4): the
//! `kind: stress` report against the published schema, its round trip and
//! an older reader, every format as a snapshot, an offline HTML page with
//! load charts, the breakpoint marker and the recovery timeline, SARIF
//! against the SARIF 2.1.0 schema, and `report diff` of two stress runs.

use std::collections::BTreeMap;

use rustman_report::bench::{BenchSecond, Checks, FileSummary, LatencySummary, StatsSummary};
use rustman_report::exploration::{
    Evidence, ReportFinding, Reproduction, Severity, SeverityCounts,
};
use rustman_report::stress::{
    BreakpointSection, FailingEndpoint, KIND_STRESS, Killed, MinimizedInput, RecoveryProbe,
    RecoverySection, StressFinding, StressProfile, StressReport, StressStep, StressSummary,
};
use rustman_report::{
    ReadError, RenderFormat, RenderOptions, Report, SCHEMA, diff, render, report_schema,
};
use serde_json::Value;

fn stats(requests: u64, p95: f64, error_rate: f64) -> StatsSummary {
    StatsSummary {
        requests,
        error_rate,
        errors: BTreeMap::from([("5xx".to_owned(), (requests as f64 * error_rate) as u64)]),
        latency: LatencySummary {
            min_ms: 1.0,
            avg_ms: p95 / 2.0,
            p50_ms: p95 / 2.0,
            p90_ms: p95 * 0.9,
            p95_ms: p95,
            p99_ms: p95 * 1.5,
            max_ms: p95 * 2.0,
        },
        checks: Checks {
            passed: requests,
            failed: 0,
        },
        ..StatsSummary::default()
    }
}

fn step(offered: f64, p95: f64, verdict: &str, index: u64) -> StressStep {
    StressStep {
        offered_rps: offered,
        achieved_rps: offered * 0.99,
        p50_ms: p95 / 2.0,
        p95_ms: p95,
        p99_ms: p95 * 1.5,
        error_rate: if verdict == "broken" { 0.02 } else { 0.0 },
        errors: BTreeMap::new(),
        verdict: verdict.to_owned(),
        failure_mode: (verdict == "broken").then(|| "latency".to_owned()),
        start_ms: index * 30_000,
        end_ms: (index + 1) * 30_000,
    }
}

fn finding(id: &str, rule: &str, severity: Severity, operation: &str) -> StressFinding {
    StressFinding {
        finding: ReportFinding {
            id: id.to_owned(),
            rule: rule.to_owned(),
            severity,
            operation: operation.to_owned(),
            description: "A malformed or mutated input made the server fail with a 5xx.".into(),
            remediation: "Validate input and answer 4xx instead of failing.".into(),
            message: "mutation input on api/orders.rman.yaml answered 500 (3 times)".into(),
            occurrences: 1,
            probes: vec!["mutation".into()],
            instance_pointer: None,
            schema_pointer: None,
            evidence: Evidence {
                method: "POST".into(),
                url: "https://staging.example.com/orders".into(),
                request_headers: vec![(
                    "Authorization".into(),
                    "Bearer «redacted:api_token»".into(),
                )],
                request_body: Some(r#"{"qty":"2","note":"<b>hi</b>"}"#.into()),
                status: 500,
                response_headers: Vec::new(),
                body: "500".into(),
                truncated: false,
                trace: None,
            },
            reproduction: Some(Reproduction {
                curl: String::new(),
                request: "version: 1\n".into(),
            }),
        },
        family: Some("mutation".into()),
        minimized: Some(MinimizedInput {
            body: Some(r#"{"qty":"2"}"#.into()),
            content_type: Some("application/json".into()),
            replays: 7,
            load_dependent: false,
        }),
    }
}

fn stress() -> StressReport {
    let mut counts = SeverityCounts::default();
    counts.add(Severity::High);
    StressReport {
        schema: SCHEMA.into(),
        kind: KIND_STRESS.into(),
        rustman_version: "0.1.0".into(),
        id: "str-20261120-abc123".into(),
        started_at: "2026-11-20T10:00:00Z".into(),
        finished_at: "2026-11-20T10:03:00Z".into(),
        target: "https://staging.example.com".into(),
        environment: Some("staging".into()),
        mode: "breakpoint".into(),
        profile: StressProfile {
            settings: BTreeMap::from([("start_rps".into(), 10.0), ("step_factor".into(), 1.5)]),
            fuzz_ratio: Some(0.1),
            malformed: Vec::new(),
            wire: Vec::new(),
            health: Some("api/health.rman.yaml".into()),
        },
        seed: 42,
        slos: vec!["p95<300ms".into(), "error_rate<1%".into()],
        overall: stats(4000, 180.0, 0.005),
        files: vec![FileSummary {
            file: "api/orders.rman.yaml".into(),
            stats: stats(4000, 180.0, 0.005),
        }],
        fuzzed: Some(stats(400, 90.0, 0.25)),
        series: vec![BenchSecond {
            second: 1,
            requests: 10,
            errors: 0,
            rps: 10.0,
            p50_ms: 10.0,
            p95_ms: 20.0,
            p99_ms: 30.0,
            active_vus: 2,
        }],
        fuzzed_series: Vec::new(),
        steps: vec![
            step(10.0, 20.0, "ok", 0),
            step(15.0, 25.0, "ok", 1),
            step(22.5, 410.0, "broken", 2),
            step(18.75, 60.0, "ok", 3),
        ],
        breakpoint: Some(BreakpointSection {
            max_sustainable_rps: Some(18.75),
            breaking_rps: Some(22.5),
            failure_mode: Some("latency".into()),
            first_failing_endpoint: Some(FailingEndpoint {
                file: "api/orders.rman.yaml".into(),
                p95_ms: 410.0,
                error_rate: 0.02,
            }),
            stop: "refined".into(),
            max_rps: 500.0,
        }),
        rate_limited_at: None,
        killed: None,
        recovery: RecoverySection {
            status: "recovered".into(),
            after_ms: Some(4000),
            baseline_ms: Some(12.0),
            health: Some("api/health.rman.yaml".into()),
            reason: None,
            last_status: Some(200),
            last_latency_ms: Some(13.0),
            timeline: (0..14)
                .map(|i| RecoveryProbe {
                    offset_ms: i * 1000,
                    status: Some(if i < 4 { 503 } else { 200 }),
                    latency_ms: if i < 4 { 900.0 } else { 13.0 },
                    good: i >= 4,
                })
                .collect(),
        },
        spike: None,
        soak: None,
        wire: Vec::new(),
        summary: StressSummary {
            findings: counts,
            requests: 4000,
            fuzzed_requests: 400,
            duration_ms: 180_000,
        },
        findings: vec![finding(
            "f001",
            "server-error-on-malformed-input",
            Severity::High,
            "api/orders.rman.yaml",
        )],
        client_saturated: false,
        interrupted: false,
        warnings: Vec::new(),
    }
}

fn report() -> Report {
    Report::Stress(Box::new(stress()))
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

/// The spec's scenario: the breakpoint fields and `recovery.status` are
/// there, the document validates and reads back.
#[test]
fn a_stress_report_validates_and_round_trips() {
    let validator =
        jsonschema::validator_for(&serde_json::from_str(&report_schema()).unwrap()).unwrap();
    let text = report().to_json();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_valid(&validator, &value);
    assert_eq!(value["kind"], "stress");
    for field in ["max_sustainable_rps", "breaking_rps", "failure_mode"] {
        assert!(value["breakpoint"].get(field).is_some(), "{field}");
    }
    assert_eq!(value["recovery"]["status"], "recovered");
    assert_eq!(
        value["findings"][0]["rule"],
        "server-error-on-malformed-input"
    );
    assert_eq!(value["findings"][0]["minimized"]["body"], r#"{"qty":"2"}"#);
    assert_eq!(Report::parse(&text).unwrap(), report());
    assert_eq!(report().kind(), "stress");
    let mut killed = stress();
    killed.killed = Some(Killed {
        rps: 180.0,
        reason: "connection errors above 50% for 5s".into(),
        message: "killed: target down at 180 req/s (connection errors above 50% for 5s)".into(),
    });
    let value: Value = serde_json::from_str(&Report::Stress(Box::new(killed)).to_json()).unwrap();
    assert_valid(&validator, &value);
    let mut broken = value.clone();
    broken["recovery"] = Value::from("fine");
    assert!(!validator.is_valid(&broken));
}

/// The spec's older reader: the document is valid `rustman.report/v1`,
/// and a reader without `stress` reports an unknown kind without crashing.
#[test]
fn an_older_reader_reports_an_unknown_kind() {
    let text = report().to_json();
    let older = ["run", "exploration", "contract", "bench"];
    let err = Report::parse_known(&text, &older).unwrap_err();
    assert!(matches!(err, ReadError::UnknownKind(ref k) if k == "stress"));
    assert!(
        err.to_string().starts_with("unknown report kind 'stress'"),
        "{err}"
    );
}

#[test]
fn snapshot_stress_formats() {
    for format in RenderFormat::ALL {
        insta::assert_snapshot!(format!("stress_{format}"), rendered(format));
    }
}

/// Task 6.4: one offline page with latency and errors against offered
/// load, the breakpoint marked, and the recovery timeline.
#[test]
fn html_is_offline_with_load_charts_a_breakpoint_marker_and_recovery() {
    let html = rendered(RenderFormat::Html);
    for external in ["src=", "href=", "url(", "@import", "<link", "<script"] {
        assert!(!html.contains(external), "{external} in the page");
    }
    assert!(!html.contains("://"), "{html}");
    assert!(html.contains("<h2>Latency against offered load</h2>"));
    assert!(html.contains("<h2>Errors against offered load</h2>"));
    assert!(html.contains("<line class=\"marker\""), "{html}");
    assert!(html.contains("<h2>Recovery timeline (recovered 4s after load stopped)</h2>"));
    assert!(html.contains("<polyline class=\"line line-latency\""));
    assert!(html.contains("&lt;b&gt;hi&lt;&#x2f;b&gt;"), "escaped input");
    assert!(html.contains("Bearer «redacted:api_token»"));
}

#[test]
fn sarif_validates_with_one_result_per_finding() {
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
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["ruleId"], "server-error-on-malformed-input");
    assert_eq!(
        results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "api/orders.rman.yaml"
    );
}

#[test]
fn junit_fails_high_findings() {
    let junit = rendered(RenderFormat::Junit);
    let doc = roxmltree::Document::parse(&junit).unwrap();
    let failures: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("failure"))
        .map(|n| n.attribute("type").unwrap())
        .collect();
    assert_eq!(failures, ["server-error-on-malformed-input"]);
}

/// Task 6.2: a second run that adds `no-recovery` diffs as new, which
/// fails at the default threshold.
#[test]
fn diff_lists_a_new_no_recovery() {
    let old = report();
    let mut later = stress();
    later.findings.push(finding(
        "f002",
        "no-recovery",
        Severity::High,
        "api/health.rman.yaml",
    ));
    let diff = diff(&old, &Report::Stress(Box::new(later))).unwrap();
    assert_eq!(diff.kind, "stress");
    let new: Vec<&str> = diff.new.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(new, ["no-recovery"]);
    assert_eq!(diff.unchanged.len(), 1);
    assert_eq!(diff.regressions(Severity::High), 1);
    assert!(diff.coverage.is_none());
}
