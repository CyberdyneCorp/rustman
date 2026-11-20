//! Bench reports (add-load-testing tasks 5.1 and 5.2): the `kind: bench`
//! report against the published schema, its round trip, every format as a
//! snapshot, an offline HTML page with both charts, and SARIF against the
//! SARIF 2.1.0 schema.

use std::collections::BTreeMap;

use rustman_report::bench::{
    Aborted, BenchProfile, BenchReport, BenchSecond, BenchTotals, Checks, ErrorSample, FileSummary,
    KIND_BENCH, LatencyKind, LatencySummary, PhaseSummary, StatsSummary, ThresholdResult,
};
use rustman_report::{RenderFormat, RenderOptions, Report, SCHEMA, render, report_schema};
use serde_json::Value;

fn stats(requests: u64, errors: &[(&str, u64)], p95: f64) -> StatsSummary {
    let errors: BTreeMap<String, u64> = errors.iter().map(|(c, n)| ((*c).into(), *n)).collect();
    let failed: u64 = errors.values().sum();
    StatsSummary {
        requests,
        error_rate: failed as f64 / requests as f64,
        errors,
        latency: LatencySummary {
            min_ms: 2.0,
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
        phases: PhaseSummary {
            dns_ms: None,
            connect_ms: Some(0.4),
            tls_ms: None,
            ttfb_ms: Some(p95 / 2.0),
            download_ms: Some(0.1),
        },
        bytes_sent: requests * 120,
        bytes_received: requests * 2,
    }
}

fn second(second: u32, rps: f64, p95: f64) -> BenchSecond {
    BenchSecond {
        second,
        requests: rps as u64,
        errors: 1,
        rps,
        p50_ms: p95 / 2.0,
        p95_ms: p95,
        p99_ms: p95 * 1.5,
        active_vus: 5,
    }
}

fn bench() -> BenchReport {
    BenchReport {
        schema: SCHEMA.into(),
        kind: KIND_BENCH.into(),
        rustman_version: "0.1.0".into(),
        started_at: "2026-11-19T10:00:00Z".into(),
        finished_at: "2026-11-19T10:00:03Z".into(),
        target: "https://staging.example.com".into(),
        environment: Some("staging".into()),
        profile: BenchProfile {
            executor: "constant-vus".into(),
            vus: Some(5),
            duration_ms: Some(3000),
            think_time: Some("0".into()),
            ..BenchProfile::default()
        },
        aborted: None,
        totals: BenchTotals {
            latency_kind: LatencyKind::Service,
            duration_ms: 3012.0,
            iterations: 60,
            iteration_duration: LatencySummary {
                min_ms: 21.0,
                avg_ms: 48.0,
                p50_ms: 45.0,
                p90_ms: 70.0,
                p95_ms: 80.0,
                p99_ms: 96.0,
                max_ms: 101.0,
            },
            interrupted: 0,
            scheduled: 0,
            dropped: 0,
            rps: 39.8,
            generator_lag_p99_ms: 1.2,
        },
        overall: stats(120, &[("5xx", 3)], 412.0),
        files: vec![
            FileSummary {
                file: "checkout/login.rman.yaml".into(),
                stats: stats(60, &[], 120.0),
            },
            FileSummary {
                file: "checkout/pay.rman.yaml".into(),
                stats: stats(60, &[("5xx", 3)], 412.0),
            },
        ],
        series: vec![
            second(1, 38.0, 300.0),
            second(2, 41.0, 412.0),
            second(3, 40.0, 380.0),
        ],
        thresholds: vec![
            ThresholdResult {
                expression: "p95<300ms".into(),
                passed: false,
                actual: "412ms".into(),
                abort: false,
            },
            ThresholdResult {
                expression: "error_rate<5%!".into(),
                passed: true,
                actual: "2.50%".into(),
                abort: true,
            },
        ],
        error_samples: vec![ErrorSample {
            class: "5xx".into(),
            file: "checkout/pay.rman.yaml".into(),
            message: "POST https://staging.example.com/pay -> 503 <b>«redacted:card»</b>".into(),
            at_ms: 1520,
        }],
        warnings: vec!["results may be generator-bound".into()],
    }
}

fn report() -> Report {
    Report::Bench(Box::new(bench()))
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
fn a_bench_report_validates_and_round_trips() {
    let validator =
        jsonschema::validator_for(&serde_json::from_str(&report_schema()).unwrap()).unwrap();
    let text = report().to_json();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_valid(&validator, &value);
    assert_eq!(Report::parse(&text).unwrap(), report());
    assert_eq!(report().kind(), "bench");
    let mut aborted = bench();
    aborted.aborted = Some(Aborted::Threshold);
    let value: Value = serde_json::from_str(&Report::Bench(Box::new(aborted)).to_json()).unwrap();
    assert_eq!(value["aborted"], "threshold");
    assert_valid(&validator, &value);
    let mut broken = value.clone();
    broken["aborted"] = Value::from("bored");
    assert!(!validator.is_valid(&broken));
}

#[test]
fn passing_needs_every_threshold_and_no_threshold_abort() {
    let mut report = bench();
    assert!(!report.passed());
    report.thresholds[0].passed = true;
    assert!(report.passed());
    report.aborted = Some(Aborted::Interrupted);
    assert!(report.passed());
    report.aborted = Some(Aborted::Threshold);
    assert!(!report.passed());
}

#[test]
fn snapshot_bench_formats() {
    for format in RenderFormat::ALL {
        insta::assert_snapshot!(format!("bench_{format}"), rendered(format));
    }
}

#[test]
fn html_is_offline_with_latency_and_rps_charts() {
    let html = rendered(RenderFormat::Html);
    for external in ["src=", "href=", "url(", "@import", "<link", "<script"] {
        assert!(!html.contains(external), "{external} in the page");
    }
    // The only URLs are the target and the sample, as escaped text.
    assert!(!html.contains("://"), "{html}");
    assert_eq!(html.matches("https:&#x2f;&#x2f;").count(), 2, "{html}");
    assert!(html.contains("<h2>Latency over time (service)</h2>"));
    assert!(html.contains("<polyline class=\"line line-p95\""));
    assert!(html.contains("<h2>Requests per second</h2>"));
    assert!(html.contains("<polyline class=\"line line-rps\""));
    assert!(html.contains("✗ p95&lt;300ms (412ms)"), "{html}");
    assert!(html.contains("&lt;b&gt;«redacted:card»"), "escaped sample");
}

#[test]
fn markdown_shows_thresholds_and_one_row_per_file() {
    let markdown = rendered(RenderFormat::Markdown);
    assert!(markdown.starts_with("# Bench report\n"), "{markdown}");
    assert!(
        markdown.contains("- ✗ p95\\<300ms \\(412ms\\)"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| `checkout/pay.rman.yaml` |"),
        "{markdown}"
    );
    assert!(markdown.contains("| `overall` |"), "{markdown}");
    assert!(markdown.contains("\\<b\\>«redacted:card»"), "{markdown}");
}

#[test]
fn junit_fails_breached_thresholds() {
    let junit = rendered(RenderFormat::Junit);
    let doc = roxmltree::Document::parse(&junit).unwrap();
    let failures: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("failure"))
        .map(|n| n.attribute("message").unwrap())
        .collect();
    assert_eq!(failures, ["✗ p95<300ms (412ms)"]);
}

#[test]
fn sarif_validates_with_one_result_per_breach() {
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
    assert_eq!(results[0]["message"]["text"], "✗ p95<300ms (412ms)");
}
