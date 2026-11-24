//! GraphQL requests under the `rustman-http` runner (add-graphql-client
//! tasks 6.1, 6.2 and 6.4): GraphQL assertions, captures from `data`,
//! history records with redacted variables and subscription events, and a
//! collection mixing http and graphql requests.

use std::fs;
use std::path::Path;

use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_graphql::{GraphqlRunner, Interactive};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_test_support::{GraphqlHttpServer, GraphqlWsServer, RecordedRequest, Reply, WsScript};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "s3cr3t-graphql-runner-canary";
const TOKEN: &str = "tok-from-login";

const LOGIN: &str = "\
version: 1
kind: graphql
name: Login
endpoint: '{{base}}/graphql'
document: 'mutation Login($password: String!) { login(password: $password) { token } }'
variables:
  password: '{{api_token}}'
assertions:
  - no_errors: true
  - data: {path: $.login.token, exists: true}
captures:
  - {var: token, from: json, path: $.data.login.token}
";

const ME: &str = "\
version: 1
kind: graphql
name: Me
endpoint: '{{base}}/graphql'
document: 'query Me { me { name email } }'
headers:
  - name: Authorization
    value: 'Bearer {{token}}'
assertions:
  - no_errors: true
  - errors_count: 1
  - error_message_contains: private
  - error_code: FORBIDDEN
  - data: {path: $.me.name, equals: Ada}
";

const HEALTH: &str = "\
version: 1
kind: http
name: Health
url: '{{base}}/health'
assertions:
  - status: 200
  - json: {path: $.ok, equals: true}
";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace with `base` pointing at `base`, a `prod` environment whose
/// secret `api_token` is [`SECRET`], and the request files `files`.
fn workspace(base: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{base}\"\n"),
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

/// Run every request of the workspace at `root` in the `prod` environment,
/// recording history.
async fn run(root: &Path) -> Vec<RequestRecord> {
    run_with(root, GraphqlRunner::default()).await
}

async fn run_with(root: &Path, graphql: GraphqlRunner) -> Vec<RequestRecord> {
    run_cancellable(root, graphql, CancellationToken::new()).await
}

/// [`run_with`], interrupted when `cancel` fires (as on Ctrl+C).
async fn run_cancellable(
    root: &Path,
    graphql: GraphqlRunner,
    cancel: CancellationToken,
) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        graphql: Some(&graphql),
        cancel,
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
    let plan = plan(&ws, &[], &[]);
    runner.run(&plan).await.requests
}

fn respond(hit: &RecordedRequest) -> Reply {
    if hit.uri == "/health" {
        return Reply::raw(200, "application/json", r#"{"ok":true}"#);
    }
    let body = hit.body_text();
    if body.contains("mutation Login") {
        return Reply::graphql(json!({"data": {"login": {"token": TOKEN}}}));
    }
    Reply::graphql(json!({
        "data": {"me": {"name": "Ada", "email": null}},
        "errors": [{
            "message": "email is private",
            "path": ["me", "email"],
            "extensions": {"code": "FORBIDDEN"},
        }],
    }))
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

#[tokio::test]
async fn graphql_assertions_fail_with_the_error_and_captures_read_data() {
    let server = GraphqlHttpServer::start(respond).await;
    let dir = workspace(
        &server.url(""),
        &[
            ("auth/1-login.rman.yaml", LOGIN),
            ("auth/2-me.rman.yaml", ME),
        ],
    );
    let records = run(dir.path()).await;
    let (login, me) = (&records[0], &records[1]);
    assert_eq!(
        login.status,
        RequestStatus::Passed,
        "{:?}",
        login.assertions
    );
    assert_eq!(login.captures[0].var, "token");
    // The captured token reached the next request.
    let sent = &server.hits()[1];
    assert_eq!(
        sent.header("authorization"),
        Some(&*format!("Bearer {TOKEN}"))
    );

    assert_eq!(me.status, RequestStatus::Failed);
    let shown: Vec<String> = me.assertions.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "no_errors: expected no errors, got 1 error: \"email is private\" at me.email",
            "pass: errors_count equals 1",
            "pass: error_message_contains \"private\"",
            "pass: error_code \"FORBIDDEN\"",
            "pass: data $.me.name equals \"Ada\"",
        ]
    );
    let graphql = me.graphql.as_ref().unwrap();
    assert_eq!(graphql.result.as_deref(), Some("partial"));
    assert_eq!(graphql.operation_type, "query");
    assert_eq!(graphql.operation_name.as_deref(), Some("Me"));
}

#[tokio::test]
async fn a_collection_mixes_http_and_graphql_requests() {
    let server = GraphqlHttpServer::start(respond).await;
    let dir = workspace(
        &server.url(""),
        &[
            ("mixed/1-health.rman.yaml", HEALTH),
            ("mixed/2-login.rman.yaml", LOGIN),
        ],
    );
    let records = run(dir.path()).await;
    assert_eq!(records.len(), 2);
    assert!(
        records.iter().all(|r| r.status == RequestStatus::Passed),
        "{records:#?}"
    );
    assert!(records[0].graphql.is_none());
    assert_eq!(
        records[1].graphql.as_ref().unwrap().operation_type,
        "mutation"
    );
    let hits: Vec<String> = server.hits().iter().map(|h| h.uri.clone()).collect();
    assert_eq!(hits, ["/health", "/graphql"]);
}

#[tokio::test]
async fn history_records_graphql_exchanges_with_secret_variables_redacted() {
    let server = GraphqlHttpServer::start(respond).await;
    let dir = workspace(&server.url(""), &[("auth/login.rman.yaml", LOGIN)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed);
    // The server did get the secret; no output path shows it.
    assert!(server.hits()[0].body_text().contains(SECRET));
    let entries = history(dir.path());
    assert_eq!(entries.len(), 1);
    let graphql = &entries[0]["record"]["graphql"];
    assert_eq!(graphql["operation_name"], "Login");
    assert_eq!(graphql["operation_type"], "mutation");
    assert_eq!(graphql["result"], "ok");
    assert_eq!(
        graphql["variables"],
        json!({"password": "«redacted:api_token»"})
    );
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
async fn history_keeps_subscription_events() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 3,
        complete: true,
    })
    .await;
    let subscription = "\
version: 1
kind: graphql
name: On message
endpoint: '{{base}}/graphql'
transport: ws
document: 'subscription OnMessage($key: String) { onMessage(key: $key) { n } }'
variables:
  key: '{{api_token}}'
assertions:
  - no_errors: true
  - data: {path: $, length: 3}
  - data: {path: '$[2].onMessage.n', equals: 3}
";
    let dir = workspace(
        &server.url("ws", ""),
        &[("live/on-message.rman.yaml", subscription)],
    );
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let entries = history(dir.path());
    let graphql = &entries[0]["record"]["graphql"];
    assert_eq!(graphql["operation_type"], "subscription");
    assert_eq!(graphql["transport"], "ws");
    assert_eq!(graphql["protocol"], "graphql-transport-ws");
    let events = &graphql["subscription"];
    assert_eq!(events["event_count"], 3);
    assert_eq!(events["ended"], "complete");
    let payloads: Vec<&Value> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["payload"]["data"]["onMessage"])
        .collect();
    assert_eq!(payloads[0]["n"], 1);
    // The server echoes the variables: the secret in them is redacted.
    assert_eq!(payloads[2]["vars"], json!({"key": "«redacted:api_token»"}));
    assert!(entries[0]["replay"].is_null());
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains(SECRET), "{text}");
}

/// Issue #119: a subscription over WebSocket records the trace ids its
/// handshake carried.
#[tokio::test]
async fn a_websocket_subscription_records_the_handshake_trace() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: true,
    })
    .await;
    let subscription = "\
version: 1
kind: graphql
name: On message
endpoint: '{{base}}/graphql'
transport: ws
document: 'subscription OnMessage { onMessage { n } }'
";
    let dir = workspace(
        &server.url("ws", ""),
        &[("on-message.rman.yaml", subscription)],
    );
    let config = dir.path().join("rustman.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}[trace]\nenabled = true\n")).unwrap();
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let trace = records[0].trace.as_ref().expect("the handshake's trace");
    let sent = server.sessions()[0]
        .header("traceparent")
        .map(str::to_owned);
    assert_eq!(
        sent,
        Some(format!("00-{}-{}-01", trace.trace_id, trace.span_id))
    );
}

#[tokio::test]
async fn an_interactive_subscription_feeds_redacted_events_until_stopped() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 3,
        complete: false,
    })
    .await;
    let subscription = "\
version: 1
kind: graphql
name: On message
endpoint: '{{base}}/graphql'
transport: ws
max_events: 1
document: 'subscription OnMessage($key: String) { onMessage(key: $key) { n } }'
variables:
  key: '{{api_token}}'
";
    let dir = workspace(
        &server.url("ws", ""),
        &[("live/on-message.rman.yaml", subscription)],
    );
    let stop = tokio_util::sync::CancellationToken::new();
    let (feed, mut events) = tokio::sync::mpsc::unbounded_channel();
    let graphql = GraphqlRunner {
        interactive: Some(Interactive {
            stop: stop.clone(),
            feed: Some(feed),
        }),
        ..GraphqlRunner::default()
    };
    let watch = async {
        let mut live = Vec::new();
        while live.len() < 3 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(20), events.recv())
                .await
                .expect("an event within 20 s")
                .expect("the feed is open");
            live.push(event.payload);
        }
        stop.cancel();
        live
    };
    let (records, live) = tokio::join!(run_with(dir.path(), graphql), watch);
    let graphql = records[0].graphql.as_ref().expect("a graphql record");
    let recorded = graphql.subscription.as_ref().unwrap();
    assert_eq!(recorded.ended, "stopped", "max_events does not bound it");
    assert_eq!(recorded.event_count, 3);
    assert_eq!(live[2]["data"]["onMessage"]["n"], 3);
    assert_eq!(
        live[0]["data"]["onMessage"]["vars"],
        json!({"key": "«redacted:api_token»"})
    );
    let text = serde_json::to_string(&live).unwrap();
    assert!(!text.contains(SECRET), "{text}");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
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

#[tokio::test]
async fn ctrl_c_keeps_the_events_a_headless_subscription_collected() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 2,
        complete: false,
    })
    .await;
    let subscription = "\
version: 1
kind: graphql
name: On message
endpoint: '{{base}}/graphql'
transport: ws
document: 'subscription { onMessage { n } }'
";
    let dir = workspace(
        &server.url("ws", ""),
        &[("live/on-message.rman.yaml", subscription)],
    );
    let cancel = CancellationToken::new();
    let ctrl_c = async {
        // Once the server sent both events (right after `subscribe`).
        let subscribed = || {
            server
                .sessions()
                .first()
                .is_some_and(|s| s.message_types().contains(&"subscribe".to_owned()))
        };
        for _ in 0..400 {
            if subscribed() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();
    };
    let (records, ()) = tokio::join!(
        run_cancellable(dir.path(), GraphqlRunner::default(), cancel.clone()),
        ctrl_c
    );
    assert_eq!(records[0].skip_reason, None, "{:?}", records[0]);
    let recorded = records[0]
        .graphql
        .as_ref()
        .unwrap()
        .subscription
        .as_ref()
        .unwrap();
    assert_eq!(recorded.ended, "stopped");
    assert_eq!(recorded.event_count, 2);
    let entries = history(dir.path());
    assert_eq!(
        entries[0]["record"]["graphql"]["subscription"]["event_count"],
        2
    );
    // The server was told the client stopped.
    let completed = || {
        server.sessions()[0]
            .message_types()
            .contains(&"complete".to_owned())
    };
    for _ in 0..100 {
        if completed() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(completed(), "{:?}", server.sessions()[0].message_types());
}

/// Regression (#67): a result over `[http] max_body_bytes` was spilled to
/// disk, read back whole, parsed and recorded. It is refused unread, and
/// neither history nor the spill folder keeps any of it.
#[tokio::test]
async fn a_result_over_max_body_bytes_is_refused_and_not_recorded() {
    const CANARY: &str = "oversized-graphql-body-canary";
    let big = CANARY.repeat(64 * 1024 / CANARY.len());
    let server =
        GraphqlHttpServer::start(move |_| Reply::graphql(json!({"data": {"login": big}}))).await;
    let dir = workspace(&server.url(""), &[("auth/login.rman.yaml", LOGIN)]);
    let settings = fs::read_to_string(dir.path().join("rustman.toml")).unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("{settings}[http]\nmax_body_bytes = 4096\n"),
    );
    let records = run(dir.path()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Errored,
        "{:?}",
        records[0]
    );
    assert_eq!(
        records[0].error.as_deref(),
        Some("HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)")
    );
    assert_eq!(server.hits().len(), 1, "an oversized result is not resent");
    let entries = history(dir.path());
    assert_eq!(entries.len(), 1);
    for file in walk(&dir.path().join(".rustman")) {
        let bytes = fs::read(&file).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(CANARY),
            "{} keeps the oversized body",
            file.display()
        );
    }
}
