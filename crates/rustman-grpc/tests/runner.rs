//! gRPC requests under the `rustman-http` runner (add-grpc-client tasks 3.5,
//! 5.1 and 5.2): the default `grpc_status: OK` expectation, status,
//! metadata and JSONPath assertions over unary and streamed responses,
//! captures from json and metadata, deadlines as request errors, and
//! history records with `protocol: grpc`, redacted.

use std::fs;
use std::path::Path;

use rustman_core::captures::CaptureOutcome;
use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_grpc::GrpcRunner;
use rustman_http::runner::{GRPC_CONFIG_ERROR, RunContext, RunOptions, Runner};
use rustman_test_support::grpc::{SESSION_COOKIE, SESSION_TOKEN};
use rustman_test_support::routes::TOKEN;
use rustman_test_support::{GrpcServer, Reflection, TestServer};
use serde_json::Value;

const SECRET: &str = "s3cr3t-grpc-runner-canary";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace with `grpc_target` and `http_url` set, a `prod`
/// environment whose secret `api_key` is [`SECRET`], and `files`.
fn workspace(grpc: &GrpcServer, http_url: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target = grpc.target();
    write(
        root,
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\ngrpc_target = \"{target}\"\nhttp_url = \"{http_url}\"\n"
        ),
    );
    write(root, "environments/prod.yaml", "secrets: [api_key]\n");
    for (rel, text) in files {
        write(root, rel, text);
    }
    let store = SecretStore::with_keychain(root, None);
    store
        .set("prod", &Secret::new("api_key", SECRET.to_owned()))
        .unwrap();
    dir
}

/// Run every request of the workspace at `root` in `prod`, with history.
async fn run(root: &Path) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let grpc = GrpcRunner::default();
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        grpc: Some(&grpc),
        ..RunOptions::default()
    };
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, options).unwrap();
    runner.run(&plan(&ws, &[], &[])).await.requests
}

fn history(root: &Path) -> Vec<Value> {
    let dir = root.join(".rustman/history");
    let mut entries: Vec<Value> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    entries.sort_by_key(|e| e["id"].as_str().unwrap().to_owned());
    entries
}

/// A `kind: grpc` file named `name` calling `method` with `rest` appended.
fn grpc_file(name: &str, method: &str, rest: &str) -> String {
    format!(
        "version: 1\nkind: grpc\nname: {name}\ntarget: '{{{{grpc_target}}}}'\nmethod: shop.v1.Catalog/{method}\n{rest}"
    )
}

/// Spec "Mixed collection": an HTTP login captures `token`, the gRPC call
/// sends it as metadata, and both exchanges are in history.
#[tokio::test(flavor = "multi_thread")]
async fn an_http_capture_feeds_grpc_metadata_and_both_reach_history() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let http = TestServer::http().await;
    let login = "version: 1
kind: http
name: login
method: GET
url: '{{http_url}}/token'
captures:
  - {var: token, from: json, path: $.access_token}
";
    let get = grpc_file(
        "get",
        "GetProduct",
        "message: {id: p-1}
metadata:
  - name: authorization
    value: 'Bearer {{token}}'
  - name: x-api-key
    value: '{{api_key}}'
assertions:
  - grpc_status: OK
  - metadata: {name: x-served-by, equals: catalog}
  - json: {path: $.id, equals: p-1}
captures:
  - {var: served_by, from: metadata, name: x-served-by}
  - {var: product, from: json, path: $.name}
",
    );
    let echo = grpc_file(
        "echo",
        "GetProduct",
        "message: {id: '{{product}}'}\nmetadata:\n  - {name: x-captured, value: '{{served_by}}'}\n",
    );
    let dir = workspace(
        &grpc,
        &http.url(""),
        &[
            ("a-login.rman.yaml", login),
            ("b-get.rman.yaml", &get),
            ("c-echo.rman.yaml", &echo),
        ],
    );
    let records = run(dir.path()).await;
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let call = &records[1];
    assert_eq!(call.status, RequestStatus::Passed, "{call:?}");
    assert_eq!(call.assertions.len(), 3);
    assert!(
        call.captures
            .iter()
            .all(|c| c.outcome == CaptureOutcome::Captured),
        "{:?}",
        call.captures
    );
    assert_eq!(records[2].status, RequestStatus::Passed, "{:?}", records[2]);
    let calls = grpc.calls();
    assert_eq!(calls[1].text("x-captured").as_deref(), Some("catalog"));
    let received = &calls[0];
    assert_eq!(
        received.text("authorization").as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(received.text("x-api-key").as_deref(), Some(SECRET));

    let entries = history(dir.path());
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["request"], "a-login.rman.yaml");
    let entry = &entries[1];
    assert_eq!(entry["request"], "b-get.rman.yaml");
    assert_eq!(entry["sent"]["method"], "GRPC");
    let url = entry["sent"]["url"].as_str().unwrap();
    assert!(url.ends_with("/shop.v1.Catalog/GetProduct"), "{url}");
    assert_eq!(entry["record"]["grpc"]["protocol"], "grpc");
    assert_eq!(entry["record"]["grpc"]["status"]["name"], "OK");
    assert!(entry["replay"].is_null(), "a gRPC call is not replayed");
    let text = entry.to_string();
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains("«redacted:api_key»"), "{text}");
}

/// Credentials a server returns in its response metadata (`set-cookie`,
/// `authorization`) are redacted in history and in the run's records, over
/// both transports, as an HTTP response's headers are.
#[tokio::test(flavor = "multi_thread")]
async fn credentials_in_response_metadata_are_redacted() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let native = grpc_file(
        "native",
        "GetProduct",
        "message: {id: session}\ncaptures:\n  - {var: sid, from: metadata, name: set-cookie}\n",
    );
    let web = grpc_file(
        "web",
        "GetProduct",
        "transport: grpc-web\nmessage: {id: session}\n",
    );
    let dir = workspace(
        &grpc,
        "http://127.0.0.1:1",
        &[("a-native.rman.yaml", &native), ("b-web.rman.yaml", &web)],
    );
    let records = run(dir.path()).await;
    for record in &records {
        assert_eq!(record.status, RequestStatus::Passed, "{record:?}");
        let text = serde_json::to_string(record).unwrap();
        assert!(!text.contains(SESSION_COOKIE), "{text}");
        assert!(!text.contains(SESSION_TOKEN), "{text}");
    }
    let entries = history(dir.path());
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        let text = entry.to_string();
        assert!(!text.contains(SESSION_COOKIE), "{text}");
        assert!(!text.contains(SESSION_TOKEN), "{text}");
        assert!(text.contains("«redacted:Authorization»"), "{text}");
    }
}

/// Spec "Endless stream bounded": a stream stopped at its bounds ends
/// `CANCELLED` by the client, and without assertions the step passes; an
/// explicit `grpc_status: OK` still fails it.
#[tokio::test(flavor = "multi_thread")]
async fn a_bounded_stream_without_assertions_passes() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let bounded = "message: {page_size: -1}\nstream:\n  max_messages: 3\n";
    let plain = grpc_file("plain", "ListProducts", bounded);
    let strict = grpc_file(
        "strict",
        "ListProducts",
        &format!("{bounded}assertions:\n  - grpc_status: OK\n"),
    );
    let dir = workspace(
        &grpc,
        "http://127.0.0.1:9",
        &[("a.rman.yaml", &plain), ("b.rman.yaml", &strict)],
    );
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    assert!(records[0].assertions.is_empty());
    let grpc_record = records[0].grpc.as_ref().unwrap();
    assert_eq!(grpc_record["status"]["name"], "CANCELLED");
    assert_eq!(grpc_record["messages"].as_array().unwrap().len(), 3);
    assert_eq!(records[1].status, RequestStatus::Failed, "{:?}", records[1]);
}

/// Spec "Status assertion failure", and the default `grpc_status: OK`.
#[tokio::test(flavor = "multi_thread")]
async fn a_status_other_than_ok_fails_the_default_expectation() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let missing = grpc_file("missing", "GetProduct", "message: {id: missing}\n");
    let expected = grpc_file(
        "expected",
        "GetProduct",
        "message: {id: missing}\nassertions:\n  - grpc_status: NOT_FOUND\n",
    );
    let dir = workspace(
        &grpc,
        "http://127.0.0.1:9",
        &[("a.rman.yaml", &missing), ("b.rman.yaml", &expected)],
    );
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Failed);
    assert_eq!(
        records[0].assertions[0].message,
        "grpc_status: expected OK, got NOT_FOUND (5)"
    );
    assert_eq!(records[1].status, RequestStatus::Passed, "{:?}", records[1]);
    let recorded = records[0].grpc.as_ref().unwrap();
    assert_eq!(recorded["status"]["message"], "product missing not found");
}

/// Spec "Assertion over streamed messages".
#[tokio::test(flavor = "multi_thread")]
async fn json_assertions_see_streamed_messages() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let list = grpc_file(
        "list",
        "ListProducts",
        "message: {}\nassertions:\n  - json: {path: $.messages, length: 3}\n  - json: {path: '$.messages[2].id', equals: p-3}\ncaptures:\n  - {var: first, from: json, path: '$.messages[0].id'}\n",
    );
    let dir = workspace(&grpc, "http://127.0.0.1:9", &[("list.rman.yaml", &list)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    assert_eq!(records[0].captures[0].outcome, CaptureOutcome::Captured);
    let timeline = records[0].grpc.as_ref().unwrap()["timeline"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(timeline, 4, "1 sent and 3 received");
}

/// Spec "Deadline exceeded": the call errors (a request error).
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_deadline_errors_the_request() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let slow = grpc_file(
        "slow",
        "GetProduct",
        "message: {id: slow}\ndeadline_ms: 200\n",
    );
    let dir = workspace(&grpc, "http://127.0.0.1:9", &[("slow.rman.yaml", &slow)]);
    let records = run(dir.path()).await;
    let record = &records[0];
    assert_eq!(record.status, RequestStatus::Errored, "{record:?}");
    assert_eq!(record.error.as_deref(), Some("grpc: DEADLINE_EXCEEDED (4)"));
    let recorded = record.grpc.as_ref().unwrap();
    assert_eq!(recorded["status"]["code"], 4);
}

/// A request that cannot be sent as written errors before any traffic,
/// with the error kind recorded.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_field_errors_before_sending() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let bad = grpc_file(
        "bad",
        "GetProduct",
        "message: {idd: p-1, note: '{{api_key}}'}\n",
    );
    let dir = workspace(&grpc, "http://127.0.0.1:9", &[("bad.rman.yaml", &bad)]);
    let records = run(dir.path()).await;
    let record = &records[0];
    assert_eq!(record.status, RequestStatus::Errored);
    assert_eq!(
        record.error.as_deref(),
        Some("grpc: unknown field 'idd' in shop.v1.GetProductRequest")
    );
    let recorded = record.grpc.as_ref().unwrap();
    assert_eq!(recorded["error"]["kind"], GRPC_CONFIG_ERROR);
    assert!(grpc.calls().is_empty(), "nothing reached the server");
    let text = history(dir.path())[0].to_string();
    assert!(!text.contains(SECRET), "{text}");
}

/// Without a gRPC executor, grpc requests are skipped as an unsupported
/// kind.
#[tokio::test(flavor = "multi_thread")]
async fn without_an_executor_grpc_requests_are_skipped() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let get = grpc_file("get", "GetProduct", "message: {id: p-1}\n");
    let dir = workspace(&grpc, "http://127.0.0.1:9", &[("get.rman.yaml", &get)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: None,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, RunOptions::default()).unwrap();
    let records = runner.run(&plan(&ws, &[], &[])).await.requests;
    assert_eq!(records[0].status, RequestStatus::Skipped);
}

/// Issue #119: a native gRPC call records the trace ids its metadata
/// carried, in the report and history, not inside the `grpc` record.
#[tokio::test(flavor = "multi_thread")]
async fn a_traced_call_records_the_metadata_trace() {
    let grpc = GrpcServer::start(Reflection::V1).await;
    let get = grpc_file("get", "GetProduct", "message: {id: p-1}\n");
    let dir = workspace(&grpc, "http://unused", &[("get.rman.yaml", &get)]);
    let config = dir.path().join("rustman.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}[trace]\nenabled = true\n")).unwrap();
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let trace = records[0].trace.as_ref().expect("the call's trace");
    let sent = grpc.calls()[0].text("traceparent");
    assert_eq!(
        sent,
        Some(format!("00-{}-{}-01", trace.trace_id, trace.span_id))
    );
    let entries = history(dir.path());
    assert_eq!(entries[0]["record"]["trace"]["trace_id"], trace.trace_id);
    assert!(entries[0]["record"]["grpc"].get("trace").is_none());
}
