//! JSON-RPC requests under the `rustman-http` runner (add-jsonrpc-client
//! tasks 2.4, 4.2, 4.3 and 5.2): assertions and captures against the
//! response envelope (per item for batches), `rpc_error`, history records
//! with redacted params and every retry attempt, and catalog validation.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, RpcRefusal, plan};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_jsonrpc::JsonrpcRunner;
use rustman_test_support::{
    JsonrpcHttpServer, JsonrpcWsServer, RecordedRequest, RpcReply, WsFrame,
};
use serde_json::{Value, json};

const SECRET: &str = "s3cr3t-jsonrpc-runner-canary";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace with `rpc_url` set to `url`, a `prod` environment whose
/// secret `api_key` is [`SECRET`], and the request files `files`.
fn workspace(url: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[variables]\nrpc_url = \"{url}\"\n"),
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
    let jsonrpc = JsonrpcRunner::default();
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        jsonrpc: Some(&jsonrpc),
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
    let records = runner.run(&plan(&ws, &[], &[])).await.requests;
    jsonrpc.close().await;
    records
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

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

fn answer(id: &Value, method: &str) -> Value {
    match method {
        "eth_blockNumber" => json!({"jsonrpc": "2.0", "id": id, "result": "0x12a05f2"}),
        "eth_chainId" => json!({"jsonrpc": "2.0", "id": id, "result": "0x1"}),
        _ => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "invalid params"}})
        }
    }
}

/// Answers each envelope (or batch) by method, echoing params in `echo`.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let body: Value = serde_json::from_slice(&hit.body).unwrap();
    let one = |e: &Value| {
        let method = e["method"].as_str().unwrap_or_default();
        let mut reply = answer(&e["id"], method);
        if method == "getBalance" || method == "echo" {
            reply = json!({"jsonrpc": "2.0", "id": e["id"], "result": e["params"]});
        }
        reply
    };
    let reply = match &body {
        Value::Array(calls) => Value::Array(calls.iter().rev().map(one).collect()),
        single => one(single),
    };
    RpcReply::json(&reply)
}

const HEAD: &str = "\
version: 1
kind: jsonrpc
name: Head
endpoint: '{{rpc_url}}'
method: eth_blockNumber
assertions:
  - json: {path: $.result, type: string}
  - rpc_error: none
captures:
  - {var: head, from: json, path: $.result}
";

const USE_HEAD: &str = "\
version: 1
kind: jsonrpc
name: UseHead
endpoint: '{{rpc_url}}'
method: echo
params: ['{{head}}']
assertions:
  - json: {path: '$.result[0]', equals: '0x12a05f2'}
";

const BAD_PARAMS: &str = "\
version: 1
kind: jsonrpc
name: BadParams
endpoint: '{{rpc_url}}'
method: eth_getBlockByNumber
params: [nope]
assertions:
  - json: {path: $.error.code, equals: -32602}
  - rpc_error: {code: -32602, message_contains: invalid}
";

#[tokio::test]
async fn captures_read_the_result_and_assertions_the_error() {
    let server = JsonrpcHttpServer::start(node).await;
    let dir = workspace(
        &server.url("/"),
        &[
            ("1-head.rman.yaml", HEAD),
            ("2-use.rman.yaml", USE_HEAD),
            ("3-bad.rman.yaml", BAD_PARAMS),
        ],
    );
    let records = run(dir.path()).await;
    for record in &records {
        assert_eq!(record.status, RequestStatus::Passed, "{record:#?}");
    }
    assert_eq!(
        server.hits()[1].json()["params"][0],
        "0x12a05f2",
        "hex kept as is"
    );
}

#[tokio::test]
async fn an_unexpected_error_fails_the_request() {
    let unexpected = "\
version: 1
kind: jsonrpc
name: Unexpected
endpoint: '{{rpc_url}}'
method: eth_getBlockByNumber
";
    let wrong = "\
version: 1
kind: jsonrpc
name: Wrong
endpoint: '{{rpc_url}}'
method: eth_chainId
assertions:
  - rpc_error: {code: -32602}
";
    let server = JsonrpcHttpServer::start(node).await;
    let files = [("a.rman.yaml", unexpected), ("b.rman.yaml", wrong)];
    let dir = workspace(&server.url("/"), &files);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Failed);
    assert_eq!(
        records[0].error.as_deref(),
        Some("rpc error -32602: invalid params")
    );
    assert_eq!(records[1].status, RequestStatus::Failed);
    assert_eq!(
        records[1].assertions[0].message,
        "rpc_error: expected code -32602, got result"
    );
}

#[tokio::test]
async fn batch_calls_are_checked_one_by_one() {
    let batch = "\
version: 1
kind: jsonrpc
name: Batch
endpoint: '{{rpc_url}}'
batch:
  - method: eth_chainId
    assertions:
      - json: {path: $.result, equals: '0x1'}
    captures:
      - {var: chain, from: json, path: $.result}
  - method: eth_blockNumber
    assertions:
      - json: {path: $.result, equals: '0x2'}
";
    let server = JsonrpcHttpServer::start(node).await;
    let dir = workspace(&server.url("/"), &[("batch.rman.yaml", batch)]);
    let records = run(dir.path()).await;
    let record = &records[0];
    assert_eq!(record.status, RequestStatus::Failed);
    assert!(record.assertions[0].passed());
    assert_eq!(
        record.assertions[1].message,
        "call 2 (eth_blockNumber): json $.result: expected \"0x2\", got \"0x12a05f2\""
    );
    assert_eq!(record.captures[0].var, "chain");
    let jsonrpc = record.jsonrpc.as_ref().unwrap();
    assert!(jsonrpc.batch);
    assert_eq!(jsonrpc.calls[1].response.as_ref().unwrap()["id"], 2);
}

#[tokio::test]
async fn history_redacts_secrets_inside_params() {
    let balance = "\
version: 1
kind: jsonrpc
name: Balance
endpoint: '{{rpc_url}}'
method: getBalance
params: {key: '{{api_key}}', commitment: finalized}
";
    let server = JsonrpcHttpServer::start(node).await;
    let dir = workspace(&server.url("/"), &[("balance.rman.yaml", balance)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    assert!(server.hits()[0].body_text().contains(SECRET));
    let entries = history(dir.path());
    assert_eq!(entries.len(), 1);
    let call = &entries[0]["record"]["jsonrpc"]["calls"][0];
    assert_eq!(
        call["request"]["params"],
        json!({"key": "«redacted:api_key»", "commitment": "finalized"})
    );
    assert_eq!(call["response"]["result"]["key"], "«redacted:api_key»");
    for file in walk(&dir.path().join(".rustman/history")) {
        let text = fs::read_to_string(&file).unwrap_or_default();
        assert!(
            !text.contains(SECRET),
            "{} leaks the secret",
            file.display()
        );
    }
    let report = serde_json::to_string(&records).unwrap();
    assert!(!report.contains(SECRET));
}

#[tokio::test]
async fn history_shows_every_retry_attempt() {
    let read = "\
version: 1
kind: jsonrpc
name: Read
endpoint: '{{rpc_url}}'
method: eth_blockNumber
idempotent: true
settings:
  retry: {attempts: 2, backoff_ms: 10}
";
    let server = JsonrpcHttpServer::start(|hit, n| match n {
        0 => RpcReply::raw(429, "text/plain", "slow down").with_header("retry-after", "0"),
        _ => node(hit, n),
    })
    .await;
    let dir = workspace(&server.url("/"), &[("read.rman.yaml", read)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed);
    let entries = history(dir.path());
    let attempts = entries[0]["record"]["jsonrpc"]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!(attempts[0]["status"], 429);
    assert_eq!(attempts[1]["status"], 200);
    assert_eq!(entries[0]["status"], 200);
}

#[tokio::test]
async fn a_gateway_error_errors_the_request() {
    let server =
        JsonrpcHttpServer::start(|_, _| RpcReply::raw(502, "text/html", "<html>bad</html>")).await;
    let dir = workspace(&server.url("/"), &[("head.rman.yaml", HEAD)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Errored);
    assert_eq!(records[0].error.as_deref(), Some("HTTP 502"));
    assert_eq!(history(dir.path())[0]["status"], 502);
}

#[tokio::test]
async fn subscription_events_are_recorded_and_assertable() {
    let subscribe = "\
version: 1
kind: jsonrpc
name: Heads
endpoint: '{{rpc_url}}'
subscription: {subscribe: eth_subscribe, unsubscribe: eth_unsubscribe, notification: eth_subscription, max_events: 3}
params: [newHeads]
assertions:
  - json: {path: $.events, length: 3}
";
    let server = JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("eth_subscribe") => {
                let mut frames = vec![WsFrame::Json(
                    json!({"jsonrpc":"2.0","id":id,"result":"0xs"}),
                )];
                for n in 0..3 {
                    frames.push(WsFrame::Pause(Duration::from_millis(5)));
                    frames.push(WsFrame::Json(json!({
                        "jsonrpc":"2.0","method":"eth_subscription",
                        "params":{"subscription":"0xs","result":{"number":n}}
                    })));
                }
                frames
            }
            _ => vec![WsFrame::Json(
                json!({"jsonrpc":"2.0","id":id,"result":true}),
            )],
        }
    })
    .await;
    let dir = workspace(&server.url("/"), &[("heads.rman.yaml", subscribe)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let entries = history(dir.path());
    let subscription = &entries[0]["record"]["jsonrpc"]["subscription"];
    assert_eq!(subscription["events"].as_array().unwrap().len(), 3);
    assert_eq!(subscription["unsubscribed"], true);
    assert!(
        entries[0]["replay"].is_null(),
        "a WebSocket exchange is not replayed"
    );
}

/// Issue #119: calls over the run's WebSocket record the trace ids of the
/// handshake that opened the connection they share.
#[tokio::test]
async fn websocket_calls_record_the_handshake_trace() {
    let call = |name: &str| {
        format!(
            "version: 1\nkind: jsonrpc\nname: {name}\nendpoint: '{{{{rpc_url}}}}'\nmethod: eth_blockNumber\n"
        )
    };
    let server = JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        vec![WsFrame::Json(
            json!({"jsonrpc":"2.0","id":id,"result":"0x1"}),
        )]
    })
    .await;
    let (a, b) = (call("A"), call("B"));
    let dir = workspace(
        &server.url("/"),
        &[("a.rman.yaml", &a), ("b.rman.yaml", &b)],
    );
    let config = dir.path().join("rustman.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}[trace]\nenabled = true\n")).unwrap();
    let records = run(dir.path()).await;
    assert_eq!(server.sessions().len(), 1, "one connection for the run");
    let sent = server.sessions()[0]
        .header("traceparent")
        .map(str::to_owned);
    for record in &records {
        assert_eq!(record.status, RequestStatus::Passed, "{record:?}");
        let trace = record.trace.as_ref().expect("the handshake's trace");
        assert_eq!(
            sent.as_deref(),
            Some(format!("00-{}-{}-01", trace.trace_id, trace.span_id).as_str())
        );
    }
}

#[tokio::test]
async fn the_catalog_refuses_unknown_methods_before_sending() {
    let typo = "\
version: 1
kind: jsonrpc
name: Typo
endpoint: '{{rpc_url}}'
method: eth_blockNumbr
";
    let server = JsonrpcHttpServer::start(node).await;
    let dir = workspace(&server.url("/"), &[("typo.rman.yaml", typo)]);
    let doc = json!({"methods": [{"name": "eth_blockNumber", "params": []}]});
    write(dir.path(), "openrpc.json", &doc.to_string());
    let toml = fs::read_to_string(dir.path().join("rustman.toml")).unwrap();
    let linked = format!("{toml}[jsonrpc.catalogs]\n\"{{{{rpc_url}}}}\" = \"openrpc.json\"\n");
    write(dir.path(), "rustman.toml", &linked);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Errored);
    assert_eq!(
        records[0].error.as_deref(),
        Some("unknown method 'eth_blockNumbr' (did you mean eth_blockNumber?)")
    );
    let jsonrpc = records[0].jsonrpc.as_ref().unwrap();
    assert_eq!(jsonrpc.refusal, Some(RpcRefusal::Validation));
    assert!(server.hits().is_empty(), "nothing is sent");
}

/// Refuses `runner_guarded_send`.
struct RunnerGuard;

impl rustman_jsonrpc::MethodGuard for RunnerGuard {
    fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
        (method == "runner_guarded_send").then(|| "runner_guarded_send is guarded".to_owned())
    }
}

#[tokio::test]
async fn a_guard_refuses_before_the_catalog_and_the_request_file() {
    let guard: std::sync::Arc<dyn rustman_jsonrpc::MethodGuard> = std::sync::Arc::new(RunnerGuard);
    rustman_jsonrpc::register_guard(guard.clone());
    let guarded = "\
version: 1
kind: jsonrpc
name: Guarded
endpoint: '{{rpc_url}}'
settings:
  validate: false
batch:
  - method: eth_chainId
  - method: runner_guarded_send
    params: ['{{api_key}}']
";
    let server = JsonrpcHttpServer::start(node).await;
    let dir = workspace(&server.url("/"), &[("guarded.rman.yaml", guarded)]);
    let records = run(dir.path()).await;
    rustman_jsonrpc::unregister_guard(&guard);
    assert_eq!(records[0].status, RequestStatus::Errored);
    assert_eq!(
        records[0].error.as_deref(),
        Some("runner_guarded_send is guarded")
    );
    let jsonrpc = records[0].jsonrpc.as_ref().unwrap();
    assert_eq!(jsonrpc.refusal, Some(RpcRefusal::Guard));
    assert!(server.hits().is_empty(), "nothing is sent");
    let shown = serde_json::to_string(&records[0]).unwrap();
    assert!(!shown.contains(SECRET), "{shown}");
}
