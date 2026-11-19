//! `rustman rpc call`, `rustman rpc batch` and `rustman rpc discover`
//! (add-jsonrpc-client tasks 5.1, 5.2 and 6.1) against in-process JSON-RPC
//! servers on their own runtime: exit codes 0, 1, 2 and 3, output formats,
//! catalogs, and secrets kept out of every output.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{closed_port, rustman, write};
use rustman_test_support::{
    JsonrpcHttpServer, JsonrpcWsServer, RecordedRequest, RpcReply, WsFrame,
};
use serde_json::{Value, json};

const SECRET: &str = "s3cr3t-jsonrpc-cli-canary";

/// The character the `big` method's result repeats.
const BIG: &str = "y";

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

/// A workspace whose `dev` environment points `rpc_url` at `url`, with the
/// secret `btc_auth` set to [`SECRET`].
fn workspace(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  rpc_url: {url}\nsecrets: [btc_auth]\n"),
    );
    rustman(dir.path())
        .args(["secret", "set", "btc_auth", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

/// A Bitcoin-Core-like node: `getblockcount`, `echo` (returns its params),
/// `rpc.discover`, and "Method not found" for anything else. Batches are
/// answered in reverse order.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let body: Value = serde_json::from_slice(&hit.body).unwrap();
    let reply = match &body {
        Value::Array(calls) => Value::Array(calls.iter().rev().filter_map(answer).collect()),
        single => match answer(single) {
            Some(reply) => reply,
            None => return RpcReply::empty(204),
        },
    };
    RpcReply::json(&reply)
}

fn answer(call: &Value) -> Option<Value> {
    let id = call.get("id")?.clone();
    let result = match call["method"].as_str().unwrap_or_default() {
        "getblockcount" => json!(800_000),
        "eth_blockNumber" => json!("0x10"),
        "echo" => call["params"].clone(),
        "big" => json!(BIG.repeat(64 * 1024)),
        "b64echo" => {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(SECRET);
            let message = format!("bad key b64={encoded}");
            let error = json!({"code": -32000, "message": message});
            return Some(json!({"jsonrpc": "2.0", "id": id, "error": error}));
        }
        "rpc.discover" => json!({
            "openrpc": "1.2.6",
            "methods": [
                {"name": "getblockcount", "params": []},
                {"name": "getblockhash", "params": [
                    {"name": "height", "required": true, "schema": {"type": "integer"}}
                ]}
            ]
        }),
        _ => {
            let error = json!({"code": -32601, "message": "Method not found"});
            return Some(json!({"jsonrpc": "2.0", "id": id, "error": error}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn history_texts(root: &Path) -> Vec<String> {
    let dir = root.join(".rustman/history");
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| fs::read_to_string(e.path()).unwrap_or_default())
        .collect()
}

#[test]
fn a_bitcoin_core_call_prints_the_result_and_redacts_the_auth_header() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let dir = workspace(&url);
    let out = rustman(dir.path())
        .args(["rpc", "call", &url, "getblockcount", "--env", "dev"])
        .args(["--header", "Authorization: Basic {{btc_auth}}"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "800000");
    let hits = served.server.hits();
    let body: Value = serde_json::from_slice(&hits[0].body).unwrap();
    assert_eq!(
        body,
        json!({"jsonrpc": "2.0", "method": "getblockcount", "id": 1})
    );
    assert_eq!(
        hits[0].header("authorization"),
        Some(format!("Basic {SECRET}").as_str())
    );
    let history = history_texts(dir.path());
    let entries = history.iter().filter(|t| t.contains("rustman.history/v1"));
    assert_eq!(entries.count(), 1);
    for shown in [text(&out.stdout), text(&out.stderr), history.concat()] {
        assert!(!shown.contains(SECRET), "{shown}");
    }
}

#[test]
fn secrets_in_params_are_redacted_in_every_output() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let dir = workspace(&url);
    for format in ["--raw", "--json", "--no-history"] {
        let out = rustman(dir.path())
            .args([
                "rpc",
                "call",
                "{{rpc_url}}",
                "echo",
                r#"{"auth":"{{btc_auth}}"}"#,
            ])
            .args(["--env", "dev", format])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let stdout = text(&out.stdout);
        assert!(stdout.contains("«redacted:btc_auth»"), "{format}: {stdout}");
        assert!(!stdout.contains(SECRET) && !text(&out.stderr).contains(SECRET));
    }
    let history = history_texts(dir.path()).concat();
    let entry: Value = history_texts(dir.path())
        .iter()
        .find_map(|t| {
            serde_json::from_str::<Value>(t)
                .ok()
                .filter(|v| v["record"]["jsonrpc"].is_object())
        })
        .unwrap();
    assert_eq!(
        entry["record"]["jsonrpc"]["calls"][0]["request"]["params"],
        json!({"auth": "«redacted:btc_auth»"})
    );
    assert!(!history.contains(SECRET));
}

/// Regression: a node echoing a secret base64-encoded in an error message
/// leaked it to stdout and history.
#[test]
fn a_base64_echo_of_a_secret_is_redacted_in_every_output() {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(SECRET);
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let dir = workspace(&url);
    for format in ["--raw", "--json"] {
        let out = rustman(dir.path())
            .args([
                "rpc",
                "call",
                "{{rpc_url}}",
                "b64echo",
                r#"["{{btc_auth}}"]"#,
            ])
            .args(["--env", "dev", format])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        let shown = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(
            shown.contains("b64=«redacted:btc_auth»"),
            "{format}: {shown}"
        );
        assert!(!shown.contains(&encoded), "{format}: {shown}");
    }
    let history = history_texts(dir.path()).concat();
    assert!(history.contains("b64=«redacted:btc_auth»"));
    assert!(!history.contains(&encoded) && !history.contains(SECRET));
}

#[test]
fn an_error_result_is_printed_and_exits_1() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let out = rustman(Path::new("."))
        .args(["rpc", "call", &url, "sendtoaddress", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        printed,
        json!({"code": -32601, "message": "Method not found"})
    );
    assert!(text(&out.stderr).contains("rpc error -32601: Method not found"));
}

/// Regression: an answer over `[http] max_body_bytes` was spilled, read
/// back whole and written to history. It is refused unread, exit 3, and
/// history keeps none of it.
#[test]
fn an_answer_over_max_body_bytes_is_refused_and_not_recorded() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let dir = workspace(&url);
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nmax_body_bytes = 4096\n",
    );
    let out = rustman(dir.path())
        .args(["rpc", "call", &url, "big"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let limit = "HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)";
    assert!(text(&out.stderr).contains(limit), "{}", text(&out.stderr));
    assert!(!text(&out.stdout).contains(&BIG.repeat(64)));
    let history = history_texts(dir.path()).concat();
    assert!(history.contains(limit), "the refusal is recorded");
    assert!(
        !history.contains(&BIG.repeat(1024)),
        "no part of the body is"
    );
}

#[test]
fn transport_failures_exit_3() {
    let served = serve(JsonrpcHttpServer::start(|_, _| {
        RpcReply::raw(502, "text/html", "<html>bad gateway</html>")
    }));
    let url = served.server.url("/");
    let out = rustman(Path::new("."))
        .args(["rpc", "call", &url, "getblockcount", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        text(&out.stderr).contains("error: HTTP 502"),
        "{}",
        text(&out.stderr)
    );
    let refused = format!("http://127.0.0.1:{}/", closed_port());
    rustman(Path::new("."))
        .args(["rpc", "call", &refused, "getblockcount", "--no-history"])
        .assert()
        .code(3);
}

#[test]
fn usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let url = "http://127.0.0.1:1/";
    for args in [
        vec!["rpc", "call", url],
        vec!["rpc", "call", url, "m", "5"],
        vec!["rpc", "call", url, "m", "[1"],
        vec!["rpc", "call", url, "m", "--subscribe", "n"],
        vec!["rpc", "call", url, "m", "--header", "nocolon"],
    ] {
        rustman(dir.path()).args(&args).assert().code(2);
    }
}

#[test]
fn notifications_ids_and_raw_output() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let notify = rustman(Path::new("."))
        .args([
            "rpc",
            "call",
            &url,
            "log",
            r#"["hi"]"#,
            "--notify",
            "--no-history",
        ])
        .output()
        .unwrap();
    assert_eq!(notify.status.code(), Some(0), "{}", text(&notify.stderr));
    assert!(notify.stdout.is_empty());
    let raw = rustman(Path::new("."))
        .args([
            "rpc",
            "call",
            &url,
            "eth_blockNumber",
            "--id",
            "req-7",
            "--raw",
            "--no-history",
        ])
        .output()
        .unwrap();
    assert_eq!(raw.status.code(), Some(0));
    assert_eq!(
        text(&raw.stdout).trim(),
        r#"{"jsonrpc":"2.0","id":"req-7","result":"0x10"}"#,
        "hex is shown as received"
    );
    let hits = served.server.hits();
    let sent: Vec<Value> = hits
        .iter()
        .map(|h| serde_json::from_slice(&h.body).unwrap())
        .collect();
    assert_eq!(
        sent[0],
        json!({"jsonrpc": "2.0", "method": "log", "params": ["hi"]})
    );
    assert_eq!(sent[1]["id"], "req-7");
}

#[test]
fn a_request_file_is_sent_with_its_assertions() {
    let served = serve(JsonrpcHttpServer::start(node));
    let dir = workspace(&served.server.url("/"));
    let file = "version: 1\nkind: jsonrpc\nname: Count\nendpoint: '{{rpc_url}}'\nmethod: getblockcount\nassertions:\n  - json: {path: $.result, equals: 1}\n";
    write(dir.path(), "count.rman.yaml", file);
    let out = rustman(dir.path())
        .args(["rpc", "call", "count.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout).trim(), "800000");
    assert!(text(&out.stderr).contains("fail: json $.result: expected 1, got 800000"));
    rustman(dir.path())
        .args(["rpc", "call", "count.rman.yaml", "getblockcount"])
        .assert()
        .code(2);
}

#[test]
fn a_piped_batch_is_sent_once_and_printed_in_input_order() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let input = "{\"method\":\"getblockcount\"}\n{\"method\":\"echo\",\"params\":[1]}\n{\"method\":\"eth_blockNumber\"}\n";
    let out = rustman(Path::new("."))
        .args(["rpc", "batch", &url, "-", "--no-history"])
        .write_stdin(input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "800000\n[1]\n\"0x10\"\n");
    let hits = served.server.hits();
    assert_eq!(hits.len(), 1);
    let sent: Value = serde_json::from_slice(&hits[0].body).unwrap();
    assert_eq!(sent.as_array().unwrap().len(), 3);
}

#[test]
fn large_batches_are_split_and_errors_exit_1() {
    let served = serve(JsonrpcHttpServer::start(node));
    let url = served.server.url("/");
    let dir = tempfile::tempdir().unwrap();
    let mut calls: Vec<Value> = (0..150)
        .map(|_| json!({"method": "getblockcount"}))
        .collect();
    calls.push(json!({"method": "nope"}));
    write(dir.path(), "calls.json", &Value::Array(calls).to_string());
    let out = rustman(dir.path())
        .args(["rpc", "batch", &url, "calls.json", "--json", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let printed: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed.len(), 151);
    assert_eq!(printed[150]["error"]["code"], -32601);
    let sizes: Vec<usize> = served
        .server
        .hits()
        .iter()
        .map(|h| serde_json::from_slice::<Vec<Value>>(&h.body).unwrap().len())
        .collect();
    assert_eq!(sizes, [100, 51]);
    write(
        dir.path(),
        "calls.yaml",
        "- method: getblockcount\n- {method: echo, params: {a: 1}}\n",
    );
    let yaml = rustman(dir.path())
        .args(["rpc", "batch", &url, "calls.yaml", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(text(&yaml.stdout), "800000\n{\"a\":1}\n");
    write(dir.path(), "bad.json", "[{\"params\":[]}]");
    rustman(dir.path())
        .args(["rpc", "batch", &url, "bad.json"])
        .assert()
        .code(2);
}

#[test]
fn discover_caches_the_catalog_that_later_calls_are_checked_against() {
    let served = serve(JsonrpcHttpServer::start(node));
    let dir = workspace(&served.server.url("/"));
    let out = rustman(dir.path())
        .args(["rpc", "discover", "{{rpc_url}}", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("2 methods cached in .rustman/cache/openrpc/"),
        "{stdout}"
    );
    let cached = fs::read_dir(dir.path().join(".rustman/cache/openrpc"))
        .unwrap()
        .count();
    assert_eq!(cached, 1);
    let typo = rustman(dir.path())
        .args(["rpc", "call", "{{rpc_url}}", "getblockcont", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(typo.status.code(), Some(2));
    assert!(
        text(&typo.stderr).contains("unknown method 'getblockcont' (did you mean getblockcount?)"),
        "{}",
        text(&typo.stderr)
    );
    let wrong = rustman(dir.path())
        .args([
            "rpc",
            "call",
            "{{rpc_url}}",
            "getblockhash",
            r#"["800000"]"#,
            "--env",
            "dev",
        ])
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(2));
    assert!(text(&wrong.stderr).contains("params[0]: expected integer, got string"));
    let before = served.server.hits().len();
    rustman(dir.path())
        .args([
            "rpc",
            "call",
            "{{rpc_url}}",
            "getblockcont",
            "--env",
            "dev",
            "--no-validate",
        ])
        .assert()
        .code(1);
    assert_eq!(
        served.server.hits().len(),
        before + 1,
        "--no-validate sends"
    );
}

#[test]
fn discover_on_a_node_without_it_exits_1() {
    let served = serve(JsonrpcHttpServer::start(|hit, _| {
        let body: Value = serde_json::from_slice(&hit.body).unwrap();
        let error = json!({"code": -32601, "message": "Method not found"});
        RpcReply::json(&json!({"jsonrpc": "2.0", "id": body["id"], "error": error}))
    }));
    let dir = workspace(&served.server.url("/"));
    let out = rustman(dir.path())
        .args(["rpc", "discover", "{{rpc_url}}", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr)
            .contains("endpoint does not support rpc.discover; provide an OpenRPC file"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_subscription_prints_its_events_and_unsubscribes() {
    let served = serve(JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        if message["method"] != "eth_subscribe" {
            return vec![WsFrame::Json(
                json!({"jsonrpc": "2.0", "id": id, "result": true}),
            )];
        }
        let mut frames = vec![WsFrame::Json(
            json!({"jsonrpc": "2.0", "id": id, "result": "0xs"}),
        )];
        for n in 0..3 {
            frames.push(WsFrame::Pause(Duration::from_millis(5)));
            frames.push(WsFrame::Json(json!({
                "jsonrpc": "2.0",
                "method": "eth_subscription",
                "params": {"subscription": "0xs", "result": {"number": n}}
            })));
        }
        frames
    }));
    let url = served.server.url("/");
    let out = rustman(Path::new("."))
        .args(["rpc", "call", &url, "eth_subscribe", r#"["newHeads"]"#])
        .args([
            "--subscribe",
            "eth_subscription",
            "--unsubscribe",
            "eth_unsubscribe",
        ])
        .args(["--max-events", "2", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "\"0xs\"");
    assert_eq!(lines.len(), 3, "{stdout}");
    let methods = served.server.sessions()[0].methods();
    assert_eq!(methods, ["eth_subscribe", "eth_unsubscribe"]);
}
