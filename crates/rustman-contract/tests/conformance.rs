//! Passive conformance (tasks 7.1, 7.2, 7.3; spec "Passive conformance of
//! recorded traffic", "Conformance coverage and unmatched traffic",
//! "Redaction in contract outputs").
//!
//! Every check here is built with [`Panicking`], a sender that panics on
//! use, and loads its document with `Spec::load_local`, the loader the
//! command uses (design D7). The command itself is checked against a live
//! port in `rustman-cli/tests/contract_verify.rs`.

use std::path::Path;

use rustman_contract::Spec;
use rustman_contract::conformance::exchange::from_history;
use rustman_contract::conformance::{
    Checker, Conformance, RecordedExchange, RecordedResponse, Verdict,
};
use rustman_contract::pact::matching::Actual;
use rustman_contract::report::{ConformanceSources, conformance_report};
use rustman_contract::sender::{SendError, Sender};
use rustman_core::history::{Entry, HistoryStore, Query};
use rustman_core::vars::ResolvedRequest;
use rustman_core::{Redactor, Secret};
use rustman_report::Report;
use serde_json::json;

/// A sender that fails the test if anything tries to reach the network.
struct Panicking;

impl Sender for Panicking {
    async fn interaction(&self, _: &ResolvedRequest) -> Result<Actual, SendError> {
        panic!("passive conformance sent an interaction")
    }

    async fn state_change(&self, _: &ResolvedRequest) -> Result<Actual, SendError> {
        panic!("passive conformance sent a state change")
    }
}

fn spec_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/contract/conformance/openapi.yaml")
        .to_string_lossy()
        .into_owned()
}

fn spec() -> Spec {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(Spec::load_local(&spec_path())).unwrap()
}

#[test]
fn the_document_loader_refuses_urls_before_connecting() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/openapi.yaml", listener.local_addr().unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(Spec::load_local(&url)).unwrap_err();
    assert!(
        error.to_string().contains("opens no network connection"),
        "{error}"
    );
    let accepted = listener.accept();
    assert!(
        matches!(&accepted, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "{accepted:?}"
    );
}

fn exchange(id: &str, method: &str, url: &str, status: u16, body: &str) -> RecordedExchange {
    RecordedExchange {
        id: id.to_owned(),
        location: format!(".rustman/history/{id}.json"),
        method: method.to_owned(),
        url: format!("http://127.0.0.1:9{url}"),
        request_headers: Vec::new(),
        request_body: None,
        response: Some(RecordedResponse {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(body.to_owned()),
        }),
    }
}

fn check(exchanges: Vec<RecordedExchange>) -> (Spec, Conformance) {
    let spec = spec();
    let result = Checker::new(&spec, &Panicking).check(exchanges);
    (spec, result)
}

#[test]
fn a_response_missing_a_required_field_is_a_violation() {
    let (_, result) = check(vec![exchange(
        "01J0000000000000000000USER",
        "GET",
        "/users/7",
        200,
        r#"{"id": 7}"#,
    )]);
    let checked = &result.checked[0];
    assert_eq!(checked.verdict, Verdict::Violation);
    assert_eq!(checked.subject, "GET /users/{id}");
    assert_eq!(checked.exchange.id, "01J0000000000000000000USER");
    assert_eq!(checked.messages, vec!["$.email: required property missing"]);
}

#[test]
fn requests_and_statuses_are_checked_too() {
    let mut post = exchange("2", "POST", "/users", 201, r#"{"id": 1, "email": "a@b"}"#);
    post.request_headers = vec![("Content-Type".into(), "application/json".into())];
    post.request_body = Some(r#"{"name": "Ana"}"#.into());
    let (_, result) = check(vec![
        exchange("1", "GET", "/users?limit=500", 200, "[]"),
        post,
        exchange("3", "GET", "/users/7", 500, "{}"),
        exchange("4", "GET", "/users/abc", 404, ""),
        exchange("5", "GET", "/users/7", 200, "not json"),
    ]);
    let messages: Vec<&[String]> = result
        .checked
        .iter()
        .map(|c| c.messages.as_slice())
        .collect();
    assert!(
        messages[0][0].starts_with("request query limit: "),
        "{messages:?}"
    );
    assert_eq!(
        messages[1][0], "request body $.email: required",
        "{messages:?}"
    );
    assert_eq!(messages[2], ["status 500 is not documented"]);
    assert!(
        messages[3][0].starts_with("request path id: "),
        "{messages:?}"
    );
    assert_eq!(messages[4], ["$: the body is not valid JSON"]);
}

#[test]
fn undocumented_traffic_is_listed_and_fails_only_when_asked() {
    let (spec, result) = check(vec![
        exchange("1", "GET", "/internal/debug", 200, "{}"),
        exchange("2", "PATCH", "/users/7", 200, "{}"),
    ]);
    assert!(
        result
            .checked
            .iter()
            .all(|c| c.verdict == Verdict::Undocumented)
    );
    assert_eq!(result.checked[0].subject, "GET /internal/debug");
    let sources = |fail_on_undocumented| ConformanceSources {
        spec: "openapi.yaml",
        exchanges: "history",
        fail_on_undocumented,
    };
    let lenient = conformance_report(&result, &spec, sources(false), &Redactor::new());
    assert!(lenient.summary.passed);
    assert_eq!(
        lenient.summary.line(),
        "0 violation, 2 undocumented, 0 conforms"
    );
    let strict = conformance_report(&result, &spec, sources(true), &Redactor::new());
    assert_eq!(strict.summary.failing, 2);
}

#[test]
fn coverage_counts_the_documented_operations_observed() {
    let ok = r#"{"id": 7, "email": "a@b"}"#;
    let (_, result) = check(vec![
        exchange("1", "GET", "/users/7", 200, ok),
        exchange("2", "GET", "/users/8", 200, r#"{"id": 8}"#),
        exchange("3", "GET", "/users", 200, "[]"),
        exchange("4", "GET", "/health", 200, "{}"),
        exchange("5", "GET", "/pets", 200, "[]"),
        exchange("6", "DELETE", "/orders/o-1", 204, ""),
    ]);
    assert_eq!(result.coverage.line(), "5/12 operations observed");
    let user = result
        .coverage
        .operations
        .iter()
        .find(|o| o.operation == "GET /users/{id}")
        .unwrap();
    assert_eq!((user.exchanges, user.violations), (2, 1));
    assert_eq!(result.count(Verdict::Conforms), 5);
}

#[test]
fn exchanges_come_from_history_entries_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 100);
    for (id, path) in [
        ("01J00000000000000000000001", "/users/7"),
        ("01J00000000000000000000002", "/health"),
    ] {
        let entry: Entry = serde_json::from_value(json!({
            "schema": "rustman.history/v1",
            "id": id,
            "timestamp": "2026-11-17T09:30:00.000Z",
            "request": "users/get.rman.yaml",
            "environment": null,
            "sent": {"method": "GET", "url": format!("http://h{path}"), "headers": [], "body": null},
            "status": 200,
            "total_ms": 1.0,
            "error": null,
            "spill": null,
            "record": {"exchange": {"response": {
                "status": 200, "http_version": "HTTP/1.1",
                "headers": [{"name": "content-type", "value": "application/json"}],
                "body": "{\"id\": 7}", "body_bytes": 9, "body_truncated": false, "binary": false}}},
            "replay": null,
        }))
        .unwrap();
        store.record(&entry).unwrap();
    }
    let (exchanges, warnings) = from_history(&store, &Query::default()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let ids: Vec<&str> = exchanges.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["01J00000000000000000000001", "01J00000000000000000000002"]
    );
    let grep = Query {
        grep: Some("health".into()),
        ..Query::default()
    };
    assert_eq!(from_history(&store, &grep).unwrap().0.len(), 1);
    let (_, result) = check(exchanges);
    assert_eq!(
        result.checked[0].messages,
        vec!["$.email: required property missing"]
    );
}

#[test]
fn exchanges_come_from_a_run_report() {
    let text = json!({
        "schema": "rustman.report/v1", "kind": "run", "rustman_version": "0.1.0",
        "started_at": "2026-11-17T09:30:00.000Z", "environment": null,
        "summary": {"total": 1, "passed": 1, "failed": 0, "errored": 0, "skipped": 0, "duration_ms": 1.0, "interrupted": false},
        "requests": [{
            "id": "users/get", "name": "get", "file": "users/get.rman.yaml", "iteration": 1,
            "status": "passed", "skip_reason": null, "error": null,
            "exchange": {
                "request": {"method": "GET", "url": "http://h/users/7", "headers": [], "body": null},
                "response": {"status": 200, "http_version": "HTTP/1.1", "headers": [], "body": "{\"id\": 7, \"email\": \"a@b\"}",
                             "body_bytes": 10, "body_truncated": false, "binary": false},
                "timings": {"dns_ms": null, "connect_ms": null, "tls_ms": null, "ttfb_ms": 1.0, "download_ms": 0.0, "total_ms": 1.0},
                "redirects": [], "insecure": false},
            "assertions": [], "captures": [], "warnings": [], "duration_ms": 1.0,
        }],
    })
    .to_string();
    let Report::Run(run) = Report::parse(&text).unwrap() else {
        panic!("a run report")
    };
    let exchanges = RecordedExchange::from_run_report(&run, "out/run.json");
    assert_eq!(exchanges[0].id, "users/get#1");
    assert_eq!(exchanges[0].location, "out/run.json#/requests/0");
    let (_, result) = check(exchanges);
    assert_eq!(result.checked[0].verdict, Verdict::Conforms);
}

#[test]
fn a_secret_echoed_in_a_body_is_redacted_in_the_evidence() {
    const TOKEN: &str = "canary-tok-91b2";
    let body = format!(r#"{{"id": "{TOKEN}", "email": "a@b"}}"#);
    let (spec, result) = check(vec![exchange("1", "GET", "/users/7", 200, &body)]);
    assert!(result.checked[0].messages[0].contains(TOKEN), "{result:?}");
    let sources = ConformanceSources {
        spec: "openapi.yaml",
        exchanges: "history",
        fail_on_undocumented: false,
    };
    let unredacted = conformance_report(&result, &spec, sources, &Redactor::new());
    let evidence = serde_json::to_string(&unredacted).unwrap();
    assert!(
        evidence.contains(TOKEN),
        "the evidence carries the echoed value"
    );
    let redactor = Redactor::new().with(&Secret::new("token", TOKEN.to_owned()));
    let report = conformance_report(&result, &spec, sources, &redactor);
    let written = serde_json::to_string(&report).unwrap();
    assert!(!written.contains(TOKEN), "{written}");
    assert!(written.contains("«redacted:token»"), "{written}");
}
