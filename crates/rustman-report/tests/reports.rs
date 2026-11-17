//! The reporters: snapshots of every format, the JSON report against its
//! published schema, and the JUnit report through an XML parser.

use rustman_core::assertions::{AssertionResult, Outcome};
use rustman_core::run::{
    ExchangeRecord, HeaderRecord, RequestRecord, RequestStatus, ResponseRecord, RunRecord,
    SentRequest, SkipReason, Summary, TimingsRecord,
};
use rustman_report::{Format, KIND_RUN, Reporter, SCHEMA, json, junit, pretty, pretty_styled, tap};

fn exchange(method: &str, status: u16) -> ExchangeRecord {
    ExchangeRecord {
        request: SentRequest {
            method: method.into(),
            url: "http://localhost:1/x".into(),
            headers: vec![HeaderRecord {
                name: "Authorization".into(),
                value: "«redacted:Authorization»".into(),
            }],
            body: None,
        },
        response: ResponseRecord {
            status,
            http_version: "HTTP/1.1".into(),
            headers: Vec::new(),
            body: Some("{}".into()),
            body_bytes: 2,
            body_truncated: false,
            binary: false,
        },
        timings: TimingsRecord {
            connect_ms: Some(0.5),
            ttfb_ms: 3.2,
            total_ms: 4.0,
            ..TimingsRecord::default()
        },
        redirects: Vec::new(),
        insecure: false,
    }
}

fn record(id: &str, status: RequestStatus) -> RequestRecord {
    let mut r = RequestRecord::new(id.into(), id.into(), format!("{id}.rman.yaml"), 1);
    r.status = status;
    r
}

/// A run with one request of every status.
fn run() -> RunRecord {
    let mut passed = record("users/list", RequestStatus::Passed);
    passed.exchange = Some(exchange("GET", 200));
    let mut failed = record("users/create", RequestStatus::Failed);
    failed.exchange = Some(exchange("POST", 404));
    failed.assertions.push(AssertionResult {
        kind: "status",
        outcome: Outcome::Fail,
        label: "status 201".into(),
        expected: Some("201".into()),
        actual: Some("404".into()),
        message: "status: expected 201, got 404".into(),
    });
    let errored = record("users/slow", RequestStatus::Errored)
        .errored("timeout after 500 ms (response)".into());
    let skipped = record("users/delete", RequestStatus::Skipped).skipped(SkipReason::Bail);
    let requests = vec![passed, failed, errored, skipped];
    RunRecord {
        started_at: "2026-11-17T09:30:00.123Z".into(),
        environment: Some("staging".into()),
        summary: Summary::of(&requests, 42.0, false),
        requests,
    }
}

fn schema() -> jsonschema::Validator {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/report.schema.json"
    );
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn the_json_report_validates_against_the_published_schema() {
    let report: serde_json::Value = serde_json::from_str(&json(&run())).unwrap();
    let validator = schema();
    let errors: Vec<String> = validator
        .iter_errors(&report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(report["schema"], SCHEMA);
    assert_eq!(report["kind"], KIND_RUN);
}

#[test]
fn the_schema_rejects_a_report_without_its_summary() {
    let mut report: serde_json::Value = serde_json::from_str(&json(&run())).unwrap();
    report.as_object_mut().unwrap().remove("summary");
    assert!(!schema().is_valid(&report));
}

#[test]
fn the_json_summary_has_every_count() {
    let report: serde_json::Value = serde_json::from_str(&json(&run())).unwrap();
    let summary = &report["summary"];
    for (key, value) in [
        ("total", 4),
        ("passed", 1),
        ("failed", 1),
        ("errored", 1),
        ("skipped", 1),
    ] {
        assert_eq!(summary[key], value, "{key}");
    }
    assert_eq!(summary["duration_ms"], 42.0);
    assert_eq!(report["requests"][3]["skip_reason"], "bail");
}

#[test]
fn the_pretty_report_lists_requests_in_order() {
    assert_eq!(
        pretty(&run()),
        "PASS  users/list  GET 200  4 ms\n\
         FAIL  users/create  POST 404  4 ms\n      status: expected 201, got 404\n\
         ERROR users/slow\n      timeout after 500 ms (response)\n\
         SKIP  users/delete  skipped (bail)\n\
         \n\
         4 requests: 1 passed, 1 failed, 1 errored, 1 skipped (42 ms)\n"
    );
    assert_eq!(Format::Pretty.render(&run()), pretty(&run()));
}

#[test]
fn an_interrupted_run_says_so() {
    let mut run = run();
    run.summary.interrupted = true;
    assert!(pretty(&run).ends_with("interrupted: the results are partial\n"));
    let report: serde_json::Value = serde_json::from_str(&json(&run)).unwrap();
    assert_eq!(report["summary"]["interrupted"], true);
    assert!(schema().is_valid(&report));
}

/// The JSON report with the version number fixed, so a release does not
/// change the snapshot.
fn json_snapshot(run: &RunRecord) -> String {
    json(run).replace(
        &format!("\"rustman_version\": \"{}\"", rustman_core::VERSION),
        "\"rustman_version\": \"[version]\"",
    )
}

#[test]
fn snapshot_pretty() {
    insta::assert_snapshot!("pretty", pretty(&run()));
}

#[test]
fn snapshot_pretty_styled() {
    insta::assert_snapshot!(
        "pretty_styled",
        pretty_styled(&run()).replace('\x1b', "\\e")
    );
}

#[test]
fn snapshot_json() {
    insta::assert_snapshot!("json", json_snapshot(&run()));
}

#[test]
fn snapshot_junit() {
    insta::assert_snapshot!("junit", junit(&run()));
}

#[test]
fn snapshot_tap() {
    insta::assert_snapshot!("tap", tap(&run()));
}

#[test]
fn styled_pretty_differs_from_plain_only_by_ansi_escapes() {
    let styled = pretty_styled(&run());
    assert!(styled.contains("\x1b["));
    assert_eq!(strip_ansi(&styled), pretty(&run()));
    assert!(!pretty(&run()).contains('\x1b'));
}

/// `text` without ANSI SGR sequences (`ESC [ ... m`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn junit_has_one_testcase_per_request_and_a_failure_per_failed_one() {
    let xml = junit(&run());
    let doc = roxmltree::Document::parse(&xml).expect("well-formed XML");
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "testsuites");
    assert_eq!(root.attribute("tests"), Some("4"));
    let cases: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .collect();
    let names: Vec<_> = cases.iter().map(|c| c.attribute("name").unwrap()).collect();
    assert_eq!(
        names,
        ["users/list", "users/create", "users/slow", "users/delete"]
    );
    let child = |i: usize, tag: &str| cases[i].children().find(|n| n.has_tag_name(tag));
    assert!(cases[0].children().all(|n| !n.is_element()));
    let failure = child(1, "failure").expect("a <failure> for the failed request");
    assert_eq!(
        failure.attribute("message"),
        Some("status: expected 201, got 404")
    );
    let error = child(2, "error").expect("an <error> for the errored request");
    assert_eq!(
        error.attribute("message"),
        Some("timeout after 500 ms (response)")
    );
    assert!(child(3, "skipped").is_some());
    assert_eq!(
        cases[1].attribute("classname"),
        Some("users/create.rman.yaml")
    );
}

#[test]
fn junit_escapes_markup_in_messages() {
    let mut run = run();
    run.requests[2].error = Some("bad <tag> & \"quote\"".into());
    let xml = junit(&run);
    let doc = roxmltree::Document::parse(&xml).expect("well-formed XML");
    let error = doc.descendants().find(|n| n.has_tag_name("error")).unwrap();
    assert_eq!(error.attribute("message"), Some("bad <tag> & \"quote\""));
}

#[test]
fn iterations_are_named_in_junit_and_tap() {
    let mut run = run();
    let mut second = run.requests[0].clone();
    second.iteration = 2;
    run.requests.push(second);
    assert!(junit(&run).contains("name=\"users/list [2]\""));
    assert!(tap(&run).contains("ok 5 - users/list [2]\n"));
}

#[test]
fn tap_counts_points_and_marks_skips() {
    let text = tap(&run());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[..2], ["TAP version 14", "1..4"]);
    assert!(text.contains("ok 4 - users/delete # SKIP bail\n"));
    assert!(text.contains("not ok 2 - users/create\n"));
}

#[test]
fn an_interrupted_tap_stream_bails_out() {
    let mut run = run();
    run.summary.interrupted = true;
    assert!(tap(&run).ends_with("Bail out! interrupted: the results are partial\n"));
}

#[test]
fn reporters_write_files_or_return_text_for_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out/nested/junit.xml");
    let reporter: Reporter = format!("junit:{}", path.display()).parse().unwrap();
    assert_eq!(reporter.write(&run()).unwrap(), None);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), junit(&run()));
    // A pretty report to a file has no color; on stdout it is styled.
    let file = dir.path().join("pretty.txt");
    let reporter: Reporter = format!("pretty:{}", file.display()).parse().unwrap();
    reporter.write(&run()).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), pretty(&run()));
    let stdout: Reporter = "pretty".parse().unwrap();
    assert_eq!(stdout.write(&run()).unwrap(), Some(pretty_styled(&run())));
    for format in [Format::Json, Format::Junit, Format::Tap] {
        assert_eq!(format.render_styled(&run()), format.render(&run()));
    }
}

/// `junit:/dev/stdout` and other devices are written in place, and a
/// symlinked report path keeps its link and updates the file it points to.
#[cfg(unix)]
#[test]
fn reporters_write_devices_in_place_and_follow_symlinks() {
    use std::os::unix::fs::FileTypeExt;

    let reporter: Reporter = "junit:/dev/null".parse().unwrap();
    assert_eq!(reporter.write(&run()).unwrap(), None);
    assert!(
        std::fs::metadata("/dev/null")
            .unwrap()
            .file_type()
            .is_char_device()
    );

    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("out/real.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "old").unwrap();
    let link = dir.path().join("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let reporter: Reporter = format!("json:{}", link.display()).parse().unwrap();
    reporter.write(&run()).unwrap();
    assert!(link.is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), json(&run()));

    // A dangling link creates its target.
    std::fs::remove_file(&real).unwrap();
    reporter.write(&run()).unwrap();
    assert!(link.is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), json(&run()));
}
