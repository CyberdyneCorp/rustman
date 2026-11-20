//! `rustman sse listen` and `rustman ws connect` (add-realtime-client task
//! 5.1) against the in-process realtime servers: exactly N JSON lines,
//! stdin lines sent as text frames and their echoes printed, a refused
//! handshake exiting with code 3, and an echoed secret printed redacted.

mod common;

use std::time::Duration;

use common::{rustman, write};
use rustman_test_support::{RealtimeSseServer, RealtimeWsServer, SseReply, WsBehavior};
use serde_json::Value;

const CANARY: &str = "cli-realtime-canary-7f3a91";

/// A runtime that serves the test servers for as long as it lives.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn json_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

#[test]
fn sse_listen_prints_exactly_max_messages_json_lines() {
    // Scenario "Pipe to jq".
    let rt = runtime();
    let events: String = (1..=5)
        .map(|n| format!("id: {n}\nevent: tick\ndata: {{\"n\":{n}}}\n\n"))
        .collect();
    let server = rt.block_on(RealtimeSseServer::start(vec![
        SseReply::events(&events).open(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["sse", "listen", &server.url("/events"), "--json"])
        .args(["--max-messages", "3"])
        .timeout(Duration::from_secs(60))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let lines = json_lines(&output.stdout);
    assert_eq!(lines.len(), 3, "{lines:?}");
    for (n, line) in lines.iter().enumerate() {
        for field in ["type", "id", "data", "t_ms"] {
            assert!(line.get(field).is_some(), "{field} in {line}");
        }
        assert_eq!(line["type"], "tick");
        assert_eq!(line["id"], (n + 1).to_string());
        assert_eq!(line["data"], format!("{{\"n\":{}}}", n + 1));
    }
}

#[test]
fn ws_connect_sends_stdin_lines_and_prints_their_echoes() {
    // Scenario "Stdin frames".
    let rt = runtime();
    let server = rt.block_on(RealtimeWsServer::start(WsBehavior::echo()));
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["ws", "connect", &server.url("/echo"), "--json"])
        .args(["--max-messages", "2"])
        .write_stdin("a\nb\n")
        .timeout(Duration::from_secs(60))
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert_eq!(server.sessions()[0].texts(), ["a", "b"], "two text frames");
    let lines = json_lines(&output.stdout);
    let data: Vec<&str> = lines.iter().map(|l| l["data"].as_str().unwrap()).collect();
    assert_eq!(data, ["a", "b"]);
    assert!(lines.iter().all(|l| l["dir"] == "received"));
}

#[test]
fn a_refused_handshake_exits_with_code_3() {
    let rt = runtime();
    let behavior = WsBehavior::default().refuse(401, "token required");
    let server = rt.block_on(RealtimeWsServer::start(behavior));
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["ws", "connect", &server.url("/feed")])
        .timeout(Duration::from_secs(60))
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: ws: handshake failed: 401 Unauthorized"),
        "{stderr}"
    );
}

#[test]
fn a_saved_request_runs_and_an_echoed_secret_is_printed_redacted() {
    let rt = runtime();
    let server = rt.block_on(RealtimeWsServer::start(WsBehavior::echo()));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(root, "environments/prod.yaml", "secrets: [api_token]\n");
    let url = server.url("/echo");
    write(
        root,
        "feed.rman.yaml",
        &format!(
            "version: 1\nkind: ws\nname: Feed\nurl: {url}\nsteps:\n- send: {{text: 'token {{{{api_token}}}}'}}\n- wait_for: {{regex: token}}\n- close: {{}}\n"
        ),
    );
    rustman(root)
        .args(["secret", "set", "api_token", "--env", "prod"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let output = rustman(root)
        .args(["ws", "connect", "feed", "--env", "prod"])
        .timeout(Duration::from_secs(60))
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert_eq!(server.sessions()[0].texts(), [format!("token {CANARY}")]);
    let shown = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(shown.contains("token «redacted:api_token»"), "{shown}");
    assert!(!shown.contains(CANARY), "{shown}");
    let wrong_kind = rustman(root)
        .args(["sse", "listen", "feed", "--env", "prod"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&wrong_kind.stderr);
    assert!(stderr.contains("is not a kind: sse request"), "{stderr}");
}

/// trace-propagation task 2.5: `--trace` on `rustman ws connect` and
/// `rustman sse listen` sends `traceparent` on the WebSocket upgrade and on
/// the SSE request.
#[test]
fn trace_flag_sends_traceparent_on_the_handshake_and_the_stream_request() {
    let rt = runtime();
    let ws = rt.block_on(RealtimeWsServer::start(WsBehavior::echo()));
    let sse = rt.block_on(RealtimeSseServer::start(vec![SseReply::events(
        "data: one\n\n",
    )]));
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["ws", "connect", &ws.url("/echo"), "--trace", "--no-history"])
        .args(["--max-messages", "1"])
        .write_stdin("a\n")
        .timeout(Duration::from_secs(60))
        .assert()
        .code(0);
    rustman(dir.path())
        .args([
            "sse",
            "listen",
            &sse.url("/events"),
            "--trace",
            "--no-history",
        ])
        .args(["--max-messages", "1"])
        .timeout(Duration::from_secs(60))
        .assert()
        .code(0);
    for value in [
        ws.sessions()[0].header("traceparent").map(str::to_owned),
        sse.hits()[0].header("traceparent").map(str::to_owned),
    ] {
        let value = value.expect("traceparent sent");
        assert!(value.starts_with("00-") && value.len() == 55, "{value}");
    }
}
