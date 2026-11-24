//! `kind: ws` and `kind: sse` requests under the `rustman-http` runner
//! (add-realtime-client tasks 1.3, 2.1, 2.2, 4.3, 4.4): resolution in a
//! workspace, history with secrets redacted, a refused handshake as a
//! request error, the TLS settings of the file, assertions and captures
//! over the received messages, and the recorded session.

use std::fs;
use std::path::Path;

use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_realtime::{FeedItem, RealtimeRunner};
use rustman_test_support::routes::SESSION;
use rustman_test_support::server::Tls;
use rustman_test_support::{
    RealtimeSseServer, RealtimeWsServer, SseReply, TestCerts, TestServer, WsAction, WsBehavior,
};
use serde_json::Value;

const SECRET: &str = "s3cr3t-realtime-runner-canary";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace with `ws_url` set to `url`, a `prod` environment whose
/// secret `api_token` is [`SECRET`], and the request files `files`.
fn workspace(url: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[variables]\nws_url = \"{url}\"\n"),
    );
    write(root, "environments/prod.yaml", "secrets: [api_token]\n");
    for (rel, text) in files {
        write(root, rel, text);
    }
    let store = SecretStore::with_keychain(root, None);
    store
        .set("prod", &Secret::new("api_token", SECRET.to_owned()))
        .unwrap();
    dir
}

async fn run(root: &Path) -> Vec<RequestRecord> {
    run_with(root, &RealtimeRunner::default()).await
}

async fn run_with(root: &Path, realtime: &RealtimeRunner) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        realtime: Some(realtime),
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
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

const FEED: &str = "\
version: 1
kind: ws
name: Feed
url: '{{ws_url}}/feed?token={{api_token}}'
headers:
- name: X-Token
  value: '{{api_token}}'
steps:
- send: {text: hello}
- wait_for: {regex: hello}
- close: {}
";

#[tokio::test]
async fn history_stores_the_session_with_the_token_redacted() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let dir = workspace(&server.url(""), &[("feed.rman.yaml", FEED)]);
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    let recorded = &server.sessions()[0];
    assert_eq!(recorded.header("x-token"), Some(SECRET));
    let entries = history(dir.path());
    assert_eq!(entries.len(), 1);
    let text = entries[0].to_string();
    assert!(!text.contains(SECRET), "{text}");
    let url = entries[0]["sent"]["url"].as_str().unwrap();
    assert!(url.ends_with("/feed?token=«redacted:api_token»"), "{url}");
    assert!(url.starts_with("ws://"), "{url}");
}

#[tokio::test]
async fn a_refused_handshake_errors_the_request() {
    let server = RealtimeWsServer::start(WsBehavior::default().refuse(401, "no")).await;
    let dir = workspace(&server.url(""), &[("feed.rman.yaml", FEED)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Errored);
    assert_eq!(
        records[0].error.as_deref(),
        Some("ws: handshake failed: 401 Unauthorized")
    );
}

#[tokio::test]
async fn step_errors_are_redacted() {
    let server = RealtimeWsServer::start(WsBehavior::default()).await;
    let file = "version: 1\nkind: ws\nname: Wait\nurl: '{{ws_url}}/'\nsteps:\n- wait_for: {regex: '{{api_token}}', timeout_ms: 200}\n";
    let dir = workspace(&server.url(""), &[("wait.rman.yaml", file)]);
    let records = run(dir.path()).await;
    let error = records[0].error.as_deref().unwrap();
    assert_eq!(
        error,
        "ws: wait_for timed out after 200 ms (regex «redacted:api_token»)"
    );
    assert!(!history(dir.path())[0].to_string().contains(SECRET));
}

#[tokio::test]
async fn the_file_tls_settings_trust_a_custom_ca() {
    let certs = TestCerts::generate();
    let server = RealtimeWsServer::start_tls(WsBehavior::echo(), &certs, Tls::Http1Only).await;
    let url = server.localhost_url("");
    let trusted = "version: 1\nkind: ws\nname: Trusted\nurl: '{{ws_url}}/'\ntls: {ca_bundle: ca.pem}\nsteps:\n- close: {}\n";
    let untrusted = "version: 1\nkind: ws\nname: Untrusted\nurl: '{{ws_url}}/'\n";
    let dir = workspace(
        &url,
        &[
            ("ca.pem", &certs.ca_pem),
            ("a-trusted.rman.yaml", trusted),
            ("b-untrusted.rman.yaml", untrusted),
        ],
    );
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    assert_eq!(records[1].status, RequestStatus::Errored);
    let error = records[1].error.as_deref().unwrap();
    assert!(error.starts_with("ws: tls: "), "{error}");
    assert!(error.contains("localhost"), "{error}");
}

#[tokio::test]
async fn without_an_executor_realtime_requests_are_skipped() {
    let dir = workspace("ws://127.0.0.1:9", &[("feed.rman.yaml", FEED)]);
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

const SNAPSHOT_FEED: &str = "\
version: 1
kind: ws
name: Ticker
url: '{{ws_url}}/ticker'
limits: {max_messages: 5}
assertions:
- messages: {count_gte: 3}
- json: {path: '$.messages[0].type', equals: snapshot}
- latency: {ttfb_lt: 10000}
captures:
- {var: first_seq, from: json, path: '$.messages[1].seq'}
";

fn ticker() -> WsBehavior {
    let mut actions = vec![WsAction::Text(r#"{"type":"snapshot","seq":0}"#.into())];
    for seq in 1..=4 {
        actions.push(WsAction::Text(format!(
            r#"{{"type":"update","seq":{seq}}}"#
        )));
    }
    WsBehavior::default().on_connect(actions)
}

#[tokio::test]
async fn message_counts_and_contents_are_asserted_and_captured() {
    let server = RealtimeWsServer::start(ticker()).await;
    let next = "version: 1\nkind: ws\nname: Next\nurl: '{{ws_url}}/'\nlimits: {max_messages: 5}\nsteps:\n- send: {text: 'seq {{first_seq}}'}\n";
    let dir = workspace(
        &server.url(""),
        &[
            ("a-ticker.rman.yaml", SNAPSHOT_FEED),
            ("b-next.rman.yaml", next),
        ],
    );
    let records = run(dir.path()).await;
    let record = &records[0];
    assert_eq!(
        record.status,
        RequestStatus::Passed,
        "{:?}",
        record.assertions
    );
    assert_eq!(record.assertions.len(), 3);
    assert_eq!(record.captures[0].var, "first_seq");
    assert_eq!(server.sessions()[1].texts(), ["seq 1"]);
    let realtime = record.realtime.as_ref().unwrap();
    assert_eq!(realtime.protocol, "ws");
    assert_eq!(realtime.received, 5);
    assert_eq!(
        realtime.end.as_ref().unwrap().reason,
        "max_messages reached (5)"
    );
}

#[tokio::test]
async fn an_unexpected_close_code_fails_the_assertion() {
    let behavior = WsBehavior::default().on_connect(vec![WsAction::Close(1011, "internal".into())]);
    let server = RealtimeWsServer::start(behavior).await;
    let file =
        "version: 1\nkind: ws\nname: Close\nurl: '{{ws_url}}/'\nassertions:\n- close_code: 1000\n";
    let dir = workspace(&server.url(""), &[("close.rman.yaml", file)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Failed);
    assert_eq!(
        records[0].assertions[0].message,
        "close_code: expected 1000, got 1011"
    );
}

#[tokio::test]
async fn history_stores_the_session_exchange() {
    let server = RealtimeWsServer::start(WsBehavior::echo().accept(&["v1.feed"])).await;
    let file = "version: 1\nkind: ws\nname: Feed\nurl: '{{ws_url}}/'\nsubprotocols: [v2.feed, v1.feed]\nsteps:\n- send: {json: {op: hello, token: '{{api_token}}'}}\n- wait_for: {json: {path: $.op, equals: hello}}\n- close: {code: 1000, reason: done}\n";
    let dir = workspace(&server.url(""), &[("feed.rman.yaml", file)]);
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    let entry = &history(dir.path())[0];
    assert!(!entry.to_string().contains(SECRET), "{entry}");
    let realtime = &entry["record"]["realtime"];
    assert_eq!(realtime["protocol"], "ws");
    assert_eq!(realtime["handshake"]["status"], 101);
    assert_eq!(realtime["handshake"]["subprotocol"], "v1.feed");
    assert_eq!(realtime["dropped_entries"], 0);
    let timeline = realtime["timeline"].as_array().unwrap();
    let kinds: Vec<(&str, &str)> = timeline
        .iter()
        .map(|e| (e["dir"].as_str().unwrap(), e["type"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds[..3],
        [
            ("system", "connect"),
            ("sent", "json"),
            ("received", "json")
        ]
    );
    assert_eq!(
        timeline[1]["payload"],
        r#"{"op":"hello","token":"«redacted:api_token»"}"#
    );
    assert_eq!(realtime["end"]["reason"], "closed by client: 1000 done");
    assert_eq!(realtime["end"]["close_code"], 1000);
    assert!(entry["replay"].is_null(), "a session is not replayable");
}

#[tokio::test]
async fn an_sse_post_stream_runs_from_a_request_file() {
    let reply =
        SseReply::events("id: 1\ndata: {\"token\":\"Hel\"}\n\nid: 2\ndata: {\"token\":\"lo\"}\n\n");
    let server = RealtimeSseServer::start(vec![reply]).await;
    let file = "\
version: 1
kind: sse
name: Chat
method: POST
url: '{{sse_url}}/v1/chat'
headers:
- {name: Authorization, value: 'Bearer {{api_token}}'}
body: {type: json, content: {stream: true}}
reconnect: {max_attempts: 0}
assertions:
- messages: {count_eq: 2}
- json: {path: '$.messages[1].token', equals: lo}
";
    let dir = workspace("ws://unused", &[("chat.rman.yaml", file)]);
    let config = dir.path().join("rustman.toml");
    let text = fs::read_to_string(&config).unwrap();
    let url = server.url("");
    fs::write(
        &config,
        text.replace(
            "[variables]\n",
            &format!("[variables]\nsse_url = \"{url}\"\n"),
        ),
    )
    .unwrap();
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?} {:?}",
        records[0].error,
        records[0].assertions
    );
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.header("accept"), Some("text/event-stream"));
    assert_eq!(hit.json(), serde_json::json!({"stream": true}));
    let entry = &history(dir.path())[0];
    assert!(!entry.to_string().contains(SECRET), "{entry}");
    let realtime = &entry["record"]["realtime"];
    assert_eq!(realtime["protocol"], "sse");
    assert_eq!(realtime["timeline"][1]["type"], "message");
    assert_eq!(realtime["timeline"][1]["id"], "1");
    assert_eq!(entry["sent"]["method"], "POST");
}

/// A workspace whose first request stores the `/session` cookie of
/// `server` in the jar, then runs `feed` (a realtime request file).
fn jar_workspace(server: &TestServer, feed: &str) -> tempfile::TempDir {
    let start = server.url("/session").replace("localhost", "127.0.0.1");
    let session = format!("version: 1\nkind: http\nname: start\nurl: '{start}'\n");
    workspace(
        "ws://unused",
        &[("1-start.rman.yaml", &session), ("2-feed.rman.yaml", feed)],
    )
}

/// The feed of a session, with the run's records.
async fn run_followed(root: &Path) -> (Vec<RequestRecord>, String) {
    let (tx, mut rx) = rustman_realtime::feed_channel();
    let realtime = RealtimeRunner {
        feed: Some(tx),
        ..RealtimeRunner::default()
    };
    let records = run_with(root, &realtime).await;
    drop(realtime);
    let mut fed = Vec::new();
    while let Some(item) = rx.recv().await {
        if let FeedItem::Entry(entry) = item {
            fed.push(serde_json::to_string(&entry).unwrap());
        }
    }
    (records, fed.join("\n"))
}

#[tokio::test]
async fn a_jar_cookie_echoed_by_an_sse_server_is_redacted_everywhere() {
    let server = TestServer::http().await;
    let echoed = format!("event: t-{SESSION}\nid: {SESSION}\ndata: cookie=session={SESSION}\n\n");
    let sse = RealtimeSseServer::start(vec![SseReply::events(&echoed)]).await;
    let feed = format!(
        "version: 1\nkind: sse\nname: Feed\nurl: '{}'\nreconnect: {{max_attempts: 0}}\nlimits: {{max_messages: 1}}\n",
        sse.url("/events")
    );
    let dir = jar_workspace(&server, &feed);
    let (records, fed) = run_followed(dir.path()).await;
    assert_eq!(
        records[1].status,
        RequestStatus::Passed,
        "{:?}",
        records[1].error
    );
    let cookie = sse.hits()[0].header("cookie").map(str::to_owned);
    assert_eq!(
        cookie,
        Some(format!("session={SESSION}")),
        "sent from the jar"
    );
    let history = history(dir.path());
    let text = serde_json::to_string(&history).unwrap();
    assert!(!text.contains(SESSION), "{text}");
    assert!(text.contains("cookie=session=«redacted:Cookie»"), "{text}");
    assert!(fed.contains("t-«redacted:Cookie»"), "{fed}");
    assert!(!fed.contains(SESSION), "{fed}");
}

#[tokio::test]
async fn a_jar_cookie_echoed_by_a_websocket_server_is_redacted_everywhere() {
    let server = TestServer::http().await;
    let ws = RealtimeWsServer::start(WsBehavior::echo()).await;
    let feed = format!(
        "version: 1\nkind: ws\nname: Feed\nurl: '{}'\nsteps:\n- send: {{text: 'cookie {SESSION}'}}\n- wait_for: {{regex: cookie}}\n- close: {{}}\n",
        ws.url("/echo")
    );
    let dir = jar_workspace(&server, &feed);
    let (records, fed) = run_followed(dir.path()).await;
    assert_eq!(
        records[1].status,
        RequestStatus::Passed,
        "{:?}",
        records[1].error
    );
    let cookie = ws.sessions()[0].header("cookie").map(str::to_owned);
    assert_eq!(
        cookie,
        Some(format!("session={SESSION}")),
        "sent from the jar"
    );
    let text = serde_json::to_string(&history(dir.path())).unwrap();
    assert!(!text.contains(SESSION), "{text}");
    assert!(text.contains("cookie «redacted:Cookie»"), "{text}");
    assert!(fed.contains("cookie «redacted:Cookie»"), "{fed}");
    assert!(!fed.contains(SESSION), "{fed}");
}

/// The trace id of a W3C `traceparent` value, checking its shape.
fn traceparent_trace_id(value: Option<&str>) -> String {
    let value = value.unwrap_or_else(|| panic!("no traceparent sent"));
    let parts: Vec<&str> = value.split('-').collect();
    assert_eq!(parts.len(), 4, "{value}");
    assert_eq!(parts[0], "00", "{value}");
    assert_eq!(parts[1].len(), 32, "{value}");
    assert_eq!(parts[2].len(), 16, "{value}");
    assert_eq!(parts[3], "01", "{value}");
    parts[1].to_owned()
}

/// trace-propagation task 2.5: with `[trace]` on, the WebSocket upgrade
/// request and the SSE request carry `traceparent`, and with `scope = "run"`
/// both share the run's trace id.
#[tokio::test]
async fn trace_context_reaches_the_websocket_handshake_and_the_sse_request() {
    let ws_server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let sse_server = RealtimeSseServer::start(vec![SseReply::events("data: one\n\n")]).await;
    let sse = "\
version: 1
kind: sse
name: Events
url: '{{sse_url}}/events'
reconnect: {max_attempts: 0}
limits: {max_messages: 1}
";
    let dir = workspace(
        &ws_server.url(""),
        &[("a-feed.rman.yaml", FEED), ("b-events.rman.yaml", sse)],
    );
    let config = dir.path().join("rustman.toml");
    let text = fs::read_to_string(&config).unwrap().replace(
        "[variables]\n",
        &format!("[variables]\nsse_url = \"{}\"\n", sse_server.url("")),
    );
    fs::write(
        &config,
        format!("{text}[trace]\nenabled = true\nscope = \"run\"\n"),
    )
    .unwrap();
    let records = run(dir.path()).await;
    for record in &records {
        assert_eq!(record.status, RequestStatus::Passed, "{:?}", record.error);
    }
    let ws_sent = ws_server.sessions()[0]
        .header("traceparent")
        .map(str::to_owned);
    let sse_sent = sse_server.hits()[0]
        .header("traceparent")
        .map(str::to_owned);
    let ws_trace = traceparent_trace_id(ws_sent.as_deref());
    let sse_trace = traceparent_trace_id(sse_sent.as_deref());
    assert_eq!(ws_trace, sse_trace, "scope = run shares one trace id");
    // Issue #119: each session's record carries its handshake's ids.
    for (record, sent) in records.iter().zip([ws_sent, sse_sent]) {
        let trace = record.trace.as_ref().expect("the handshake's trace");
        let sent = sent.unwrap();
        assert_eq!(
            sent,
            format!("00-{}-{}-01", trace.trace_id, trace.span_id),
            "{}",
            record.name
        );
        assert!(trace.sampled);
        assert!(trace.end_ms >= trace.start_ms);
    }
}

/// Without `[trace]`, realtime requests carry no trace context.
#[tokio::test]
async fn realtime_requests_carry_no_trace_context_by_default() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let dir = workspace(&server.url(""), &[("feed.rman.yaml", FEED)]);
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    assert_eq!(server.sessions()[0].header("traceparent"), None);
    assert_eq!(records[0].trace, None);
}
