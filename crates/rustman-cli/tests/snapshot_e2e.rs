//! Snapshot testing end to end (snapshot-testing task 7.1): an `http` and a
//! `graphql` request in nested folders are recorded, a mutated response
//! fails in CI mode, `--update-snapshots` fixes it, and CI mode then
//! passes. Snapshot paths are printed with `/` on every platform. `jsonrpc`
//! requests (a single call and a batch) run the same cycle against the
//! JSON-RPC stub. The `mcp` kind is covered with stub exchanges in
//! `crates/rustman-core/tests/snapshot_kinds.rs`.

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use common::{Server, request, write};
use rustman_test_support::{
    GraphqlHttpServer, JsonrpcHttpServer, JsonrpcWsServer, RecordedRequest, Reply, RpcReply,
    WsFrame,
};
use serde_json::{Value, json};

const HTTP_SNAPSHOT: &str = "api/users/__snapshots__/get-user.default.snap.yaml";
const GRAPHQL_SNAPSHOT: &str = "api/graph/__snapshots__/user.default.snap.yaml";

/// `rustman` without the caller's `CI` variable, which turns CI mode on.
fn rustman(dir: &Path) -> Command {
    let mut cmd = common::rustman(dir);
    cmd.env_remove("CI");
    cmd
}

/// The GraphQL server's answer: the `v` variable echoed back.
fn respond(hit: &RecordedRequest) -> Reply {
    let v = hit.json()["variables"]["v"].clone();
    Reply::graphql(json!({"data": {"user": {"name": "Ada", "v": v}}}))
}

/// A workspace with `api/users/get-user` (http, `/echo?v={{v}}`) and
/// `api/graph/user` (graphql, variable `v`), both with a snapshot.
fn workspace(http: &Server, graphql: &GraphqlHttpServer) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\n  graph: {}\n  v: '1'\n",
            http.url(""),
            graphql.url("")
        ),
    );
    write(
        root,
        "api/users/get-user.rman.yaml",
        &request(
            "get-user",
            "{{base_url}}/echo?v={{v}}",
            "assertions:\n  - status: 200\n  - snapshot\n",
        ),
    );
    write(
        root,
        "api/graph/user.rman.yaml",
        "version: 1\nkind: graphql\nname: user\nendpoint: '{{graph}}/graphql'\n\
         document: 'query User($v: String) { user(v: $v) { name v } }'\n\
         variables:\n  v: '{{v}}'\nassertions:\n  - snapshot\n",
    );
    ws
}

fn run(ws: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = rustman(ws)
        .args(["run", "api", "--env", "dev"])
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), text)
}

fn files(ws: &Path) -> [String; 2] {
    [HTTP_SNAPSHOT, GRAPHQL_SNAPSHOT].map(|p| fs::read_to_string(ws.join(p)).unwrap())
}

/// Run with `args`, expect the exit `code` and every one of `needles`.
fn expect(ws: &Path, args: &[&str], code: i32, needles: &[&str]) {
    let (actual, out) = run(ws, args);
    assert_eq!(actual, Some(code), "{out}");
    for needle in needles {
        assert!(out.contains(needle), "missing {needle:?}: {out}");
    }
}

#[test]
fn record_fail_in_ci_then_update_over_http_and_graphql() {
    let http = Server::http();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let graphql = runtime.block_on(GraphqlHttpServer::start(respond));
    let ws = workspace(&http, &graphql);
    let ws = ws.path();

    // Record; paths use '/' on every platform.
    expect(
        ws,
        &[],
        0,
        &["2 snapshots recorded", HTTP_SNAPSHOT, GRAPHQL_SNAPSHOT],
    );
    let recorded = files(ws);
    assert!(recorded[0].contains("kind: http"), "{}", recorded[0]);
    assert!(recorded[1].contains("kind: graphql"), "{}", recorded[1]);

    // A mutated response fails in CI mode and writes nothing.
    let http_mismatch = format!("snapshot mismatch: {HTTP_SNAPSHOT}");
    let graphql_mismatch = format!("snapshot mismatch: {GRAPHQL_SNAPSHOT}");
    expect(
        ws,
        &["--ci", "--var", "v=2"],
        1,
        &[&http_mismatch, &graphql_mismatch, "2 snapshots failed"],
    );
    assert_eq!(files(ws), recorded);

    // Updating fixes it.
    expect(ws, &["-u", "--var", "v=2"], 0, &["2 snapshots updated"]);
    let updated = files(ws);
    assert_ne!(updated, recorded);

    // CI mode (here from the `CI` variable) now matches.
    let output = rustman(ws)
        .args(["run", "api", "--env", "dev", "--var", "v=2"])
        .env("CI", "true")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{out}");
    assert!(out.contains("2 snapshots matched"), "{out}");
    assert_eq!(files(ws), updated);
    drop(graphql);
}

#[test]
fn graphql_query_honors_ci_mode_and_notes_recorded_snapshots() {
    let http = Server::http();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let graphql = runtime.block_on(GraphqlHttpServer::start(respond));
    let ws = workspace(&http, &graphql);
    let ws = ws.path();
    let query = |args: &[&str], ci: Option<&str>| {
        let mut cmd = rustman(ws);
        cmd.args([
            "graphql",
            "query",
            "api/graph/user.rman.yaml",
            "--env",
            "dev",
        ])
        .args(args);
        if let Some(value) = ci {
            cmd.env("CI", value);
        }
        let output = cmd.output().unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let missing = format!("snapshot missing: {GRAPHQL_SNAPSHOT}");
    for (args, ci) in [(&["--ci"][..], None), (&[][..], Some("1"))] {
        let (code, err) = query(args, ci);
        assert_eq!(code, Some(1), "{err}");
        assert!(err.contains(&missing), "{err}");
    }
    assert!(
        !ws.join(GRAPHQL_SNAPSHOT).exists(),
        "CI mode wrote a snapshot"
    );
    let (code, err) = query(&[], None);
    assert_eq!(code, Some(0), "{err}");
    let note = format!("snapshot default recorded: {GRAPHQL_SNAPSHOT}");
    assert!(err.contains(&note), "{err}");
    drop(graphql);
}

const RPC_SNAPSHOT: &str = "rpc/__snapshots__/balance.default.snap.yaml";
const BATCH_FIRST: &str = "rpc/__snapshots__/batch.first.snap.yaml";
const BATCH_ERROR: &str = "rpc/__snapshots__/batch.refused.snap.yaml";

/// The JSON-RPC stub's answer to one call: `getBalance` echoes its first
/// param, `refuse` fails with an error object.
fn rpc_answer(call: &Value) -> Value {
    let id = call["id"].clone();
    match call["method"].as_str() {
        Some("refuse") => json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32000, "message": "refused"},
        }),
        _ => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"balance": call["params"][0].clone()},
        }),
    }
}

fn rpc_respond(hit: &RecordedRequest, _: usize) -> RpcReply {
    let body = hit.json();
    let answer = match &body {
        Value::Array(calls) => Value::Array(calls.iter().map(rpc_answer).collect()),
        call => rpc_answer(call),
    };
    RpcReply::json(&answer)
}

/// A workspace with `rpc/balance` (a single call, variable `v`) and
/// `rpc/batch` (a call snapshotting its result and one its error).
fn rpc_workspace(endpoint: &str) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!("variables:\n  rpc_url: {endpoint}\n  v: '1'\n"),
    );
    write(
        root,
        "rpc/balance.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: balance\nendpoint: '{{rpc_url}}'\n\
         method: getBalance\nparams: ['{{v}}']\nassertions:\n  - snapshot\n",
    );
    write(
        root,
        "rpc/batch.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: batch\nendpoint: '{{rpc_url}}'\nbatch:\n\
         \x20 - method: getBalance\n    params: ['{{v}}']\n\
         \x20   assertions:\n      - snapshot: {name: first}\n\
         \x20 - method: refuse\n    assertions:\n      - rpc_error: {code: -32000}\n\
         \x20     - snapshot: {name: refused}\n",
    );
    ws
}

fn rpc_files(ws: &Path) -> [String; 3] {
    [RPC_SNAPSHOT, BATCH_FIRST, BATCH_ERROR].map(|p| fs::read_to_string(ws.join(p)).unwrap())
}

/// Run `rustman <args>` in `ws`; the exit code and stdout plus stderr.
fn rustman_out(ws: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = rustman(ws).args(args).output().unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), text)
}

/// Snapshot assertions of `kind: jsonrpc` requests (#72): a single call
/// and the calls of a batch (a result and an error object) are recorded,
/// a changed result fails in CI mode without writing, `--update-snapshots`
/// fixes it, and `rustman snapshot list` sees every file.
#[test]
fn jsonrpc_snapshots_record_fail_in_ci_then_update() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(JsonrpcHttpServer::start(rpc_respond));
    let ws = rpc_workspace(&server.url("/"));
    let ws = ws.path();
    let run = |args: &[&str]| {
        let mut all = vec!["run", "rpc", "--env", "dev"];
        all.extend_from_slice(args);
        rustman_out(ws, &all)
    };

    let (code, out) = run(&[]);
    assert_eq!(code, Some(0), "{out}");
    for needle in [
        "3 snapshots recorded",
        RPC_SNAPSHOT,
        BATCH_FIRST,
        BATCH_ERROR,
    ] {
        assert!(out.contains(needle), "missing {needle:?}: {out}");
    }
    let recorded = rpc_files(ws);
    assert!(recorded[0].contains("kind: jsonrpc"), "{}", recorded[0]);
    assert!(recorded[0].contains("balance: '1'"), "{}", recorded[0]);
    assert!(recorded[2].contains("message: refused"), "{}", recorded[2]);

    // A changed result fails in CI mode and writes nothing; the error
    // object did not change.
    let (code, out) = run(&["--ci", "--var", "v=2"]);
    assert_eq!(code, Some(1), "{out}");
    for needle in [
        format!("snapshot mismatch: {RPC_SNAPSHOT}"),
        format!("snapshot mismatch: {BATCH_FIRST}"),
        "2 snapshots failed".to_owned(),
    ] {
        assert!(out.contains(&needle), "missing {needle:?}: {out}");
    }
    assert_eq!(rpc_files(ws), recorded);

    // Updating fixes it, and CI mode then matches.
    let (code, out) = run(&["-u", "--var", "v=2"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("2 snapshots updated"), "{out}");
    let updated = rpc_files(ws);
    assert_ne!(updated, recorded);
    assert_eq!(updated[2], recorded[2]);
    let (code, out) = run(&["--ci", "--var", "v=2"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("3 snapshots matched"), "{out}");

    // `rpc call` checks the same snapshot.
    let (code, out) = rustman_out(
        ws,
        &[
            "rpc",
            "call",
            "rpc/balance.rman.yaml",
            "--env",
            "dev",
            "--ci",
        ],
    );
    assert_eq!(code, Some(1), "{out}");

    // The inventory sees every file as in use.
    let (code, out) = rustman_out(ws, &["snapshot", "list"]);
    assert_eq!(code, Some(0), "{out}");
    for path in [RPC_SNAPSHOT, BATCH_FIRST, BATCH_ERROR] {
        let line = out.lines().find(|l| l.contains(path));
        assert!(line.is_some_and(|l| l.contains("ok")), "{path}: {out}");
    }
    drop(server);
}

/// The stub's answer to a WebSocket message: a subscription id, then two
/// notifications; `true` to the unsubscribe.
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
    for n in 0..2 {
        frames.push(WsFrame::Json(json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {"subscription": "0xs", "result": {"number": n}}
        })));
    }
    frames
}

/// A JSON-RPC subscription snapshots its event list, as GraphQL
/// subscriptions do.
#[test]
fn a_jsonrpc_subscription_snapshots_its_events() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(JsonrpcWsServer::start(heads));
    let ws = rpc_workspace(&server.url("/"));
    let ws = ws.path();
    fs::remove_file(ws.join("rpc/balance.rman.yaml")).unwrap();
    fs::remove_file(ws.join("rpc/batch.rman.yaml")).unwrap();
    write(
        ws,
        "rpc/heads.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: heads\nendpoint: '{{rpc_url}}'\nparams: [newHeads]\n\
         subscription:\n  subscribe: eth_subscribe\n  unsubscribe: eth_unsubscribe\n\
         \x20 notification: eth_subscription\n  max_events: 2\n\
         assertions:\n  - snapshot\n",
    );
    let (code, out) = rustman_out(ws, &["run", "rpc", "--env", "dev"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("1 snapshot recorded"), "{out}");
    let file = fs::read_to_string(ws.join("rpc/__snapshots__/heads.default.snap.yaml")).unwrap();
    assert!(file.contains("events:"), "{file}");
    assert!(file.contains("number: 1"), "{file}");
    let (code, out) = rustman_out(ws, &["run", "rpc", "--env", "dev", "--ci"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("1 snapshot matched"), "{out}");
    drop(server);
}
