//! `rustman run` over a collection of `kind: jsonrpc` requests
//! (add-jsonrpc-client task 8.1): a call, a batch, a subscription and an
//! error assertion against in-process stub servers, with the JSON report
//! carrying `jsonrpc` exchanges in the `rustman.report/v1` format.
//!
//! The same collection also runs against a local Anvil node when
//! `RUSTMAN_ANVIL_URL` (its HTTP URL; the WebSocket URL is the same with
//! `ws://`) is set, as the CI `jsonrpc-anvil` job does; without it that
//! test passes without doing anything.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{rustman, write};
use rustman_test_support::{
    JsonrpcHttpServer, JsonrpcWsServer, RecordedRequest, RpcReply, WsFrame,
};
use serde_json::{Value, json};

const SECRET: &str = "s3cr3t-jsonrpc-run-canary";

/// A server and the runtime it is served from (dropped after it).
struct Served<S> {
    server: S,
    _runtime: tokio::runtime::Runtime,
}

fn serve<S>(start: impl Future<Output = S>) -> Served<S> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(start);
    Served {
        server,
        _runtime: runtime,
    }
}

/// The collection: a call capturing the head, a batch, an error asserted
/// with `rpc_error`, and a `newHeads` subscription bounded to 2 events.
const FILES: &[(&str, &str)] = &[
    (
        "chain/01-head.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Head\nendpoint: '{{rpc_url}}'\n\
         method: eth_blockNumber\nheaders:\n  - name: X-Api-Key\n    value: '{{api_key}}'\n\
         assertions:\n  - rpc_error: none\n  - json: {path: '$.result', matches: '^0x[0-9a-f]+$'}\n\
         captures:\n  - {var: head, from: json, path: '$.result'}\n",
    ),
    (
        "chain/02-batch.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Batch\nendpoint: '{{rpc_url}}'\nbatch:\n\
         \x20 - method: eth_chainId\n    assertions:\n      - json: {path: '$.result', exists: true}\n\
         \x20 - method: eth_getBlockByNumber\n    params: ['{{head}}', false]\n\
         \x20   assertions:\n      - rpc_error: none\n",
    ),
    (
        "chain/03-unknown.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Unknown\nendpoint: '{{rpc_url}}'\n\
         method: rustman_noSuchMethod\nassertions:\n  - rpc_error: {code: -32601}\n",
    ),
    (
        "chain/04-heads.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Heads\nendpoint: '{{ws_url}}'\nparams: [newHeads]\n\
         subscription:\n  subscribe: eth_subscribe\n  unsubscribe: eth_unsubscribe\n\
         \x20 notification: eth_subscription\n  max_events: 2\n\
         assertions:\n  - json: {path: '$.events', length: 2}\n",
    ),
];

/// A workspace with the collection, whose `dev` environment points at
/// `http` and `ws`, with the secret `api_key` set to [`SECRET`].
fn workspace(http: &str, ws: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!("variables:\n  rpc_url: {http}\n  ws_url: {ws}\nsecrets: [api_key]\n"),
    );
    for (rel, text) in FILES {
        write(root, rel, text);
    }
    rustman(root)
        .args(["secret", "set", "api_key", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

/// An EVM-like node over HTTP: batches answered in reverse order.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let body: Value = serde_json::from_slice(&hit.body).unwrap();
    let reply = match &body {
        Value::Array(calls) => Value::Array(calls.iter().rev().map(answer).collect()),
        single => answer(single),
    };
    RpcReply::json(&reply)
}

fn answer(call: &Value) -> Value {
    let id = call["id"].clone();
    let result = match call["method"].as_str().unwrap_or_default() {
        "eth_blockNumber" => json!("0x12a05f2"),
        "eth_chainId" => json!("0x7a69"),
        "eth_getBlockByNumber" => json!({"number": call["params"][0]}),
        _ => {
            let error = json!({"code": -32601, "message": "Method not found"});
            return json!({"jsonrpc": "2.0", "id": id, "error": error});
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A WebSocket node streaming `newHeads` every 5 ms after subscribing.
fn heads(message: &Value) -> Vec<WsFrame> {
    let id = message["id"].clone();
    if message["method"] != "eth_subscribe" {
        return vec![WsFrame::Json(
            json!({"jsonrpc": "2.0", "id": id, "result": true}),
        )];
    }
    let mut frames = vec![WsFrame::Json(
        json!({"jsonrpc": "2.0", "id": id, "result": "0xs"}),
    )];
    for n in 0..5 {
        frames.push(WsFrame::Pause(Duration::from_millis(5)));
        frames.push(WsFrame::Json(json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {"subscription": "0xs", "result": {"number": n}}
        })));
    }
    frames
}

/// `rustman run --reporter json` in `root`: the exit code and the report.
fn run_report(root: &Path) -> (Option<i32>, Value, String) {
    let out = rustman(root)
        .args(["run", "--env", "dev", "--reporter", "json"])
        .timeout(Duration::from_secs(120))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let report =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}\n{stdout}\n{stderr}"));
    (out.status.code(), report, format!("{stdout}{stderr}"))
}

/// The report matches the published `rustman.report/v1` schema.
fn assert_valid(report: &Value) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/report.schema.json"
    );
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

fn request<'r>(report: &'r Value, name: &str) -> &'r Value {
    report["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no request {name}"))
}

/// The checks shared by the stub and the Anvil runs.
fn check_report(code: Option<i32>, report: &Value, output: &str) {
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(report["schema"], "rustman.report/v1");
    assert_eq!(report["summary"]["passed"], 4, "{output}");
    assert_valid(report);
    let head = &request(report, "Head")["jsonrpc"];
    assert_eq!(head["transport"], "http");
    assert_eq!(head["calls"][0]["outcome"], "result");
    let batch = &request(report, "Batch")["jsonrpc"];
    assert_eq!(batch["batch"], true);
    assert_eq!(batch["calls"].as_array().unwrap().len(), 2);
    let unknown = &request(report, "Unknown")["jsonrpc"];
    assert_eq!(unknown["calls"][0]["outcome"], "error");
    let heads = &request(report, "Heads")["jsonrpc"];
    assert_eq!(heads["transport"], "ws");
    assert_eq!(heads["subscription"]["ended"], "max_events");
    assert_eq!(heads["subscription"]["events"].as_array().unwrap().len(), 2);
    assert_eq!(heads["subscription"]["unsubscribed"], true);
    assert!(!output.contains(SECRET), "the secret leaked: {output}");
}

#[test]
fn a_run_reports_calls_batches_subscriptions_and_errors() {
    let http = serve(JsonrpcHttpServer::start(node));
    let ws = serve(JsonrpcWsServer::start(heads));
    let dir = workspace(&http.server.url("/"), &ws.server.url("/"));
    let (code, report, output) = run_report(dir.path());
    check_report(code, &report, &output);
    let batch = request(&report, "Batch");
    assert_eq!(
        batch["jsonrpc"]["calls"][1]["request"]["params"][0], "0x12a05f2",
        "the capture of Head reached the batch"
    );
    let sent = http.server.hits()[0].header("x-api-key").map(str::to_owned);
    assert_eq!(sent.as_deref(), Some(SECRET), "the secret is sent");
    let methods = ws.server.sessions()[0].methods();
    assert_eq!(methods, ["eth_subscribe", "eth_unsubscribe"]);
}

#[test]
fn a_gateway_error_exits_3() {
    let http = serve(JsonrpcHttpServer::start(|_, _| {
        RpcReply::raw(502, "text/html", "<html>bad gateway</html>")
    }));
    let dir = workspace(&http.server.url("/"), "ws://127.0.0.1:9/");
    let out = rustman(dir.path())
        .args(["run", "--env", "dev", "chain/01-head.rman.yaml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stdout).contains("HTTP 502"));
}

#[test]
fn the_collection_runs_against_anvil_when_present() {
    let Ok(http) = std::env::var("RUSTMAN_ANVIL_URL") else {
        eprintln!("RUSTMAN_ANVIL_URL is not set: skipping the Anvil run");
        return;
    };
    let ws = http.replacen("http", "ws", 1);
    let dir = workspace(&http, &ws);
    let (code, report, output) = run_report(dir.path());
    check_report(code, &report, &output);
}
