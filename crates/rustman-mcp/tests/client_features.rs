//! The MCP client's operations against the scripted fixture server
//! (`tests/support/mcp_fixture.rs`): tools, resources, prompts and
//! completion; notifications, progress, timeout and cancellation; and
//! sampling, elicitation and roots through the approval brokers.

// rmcp marks logging and sampling deprecated (SEP-2577); the client
// negotiates revisions that have them.
#![allow(deprecated)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::LoggingLevel;
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::Resolver;
use rustman_mcp::client::{
    Approval, ApprovalBroker, CallOptions, ClientError, CompletionTarget, ConnectOptions,
    HeadlessBroker, InteractiveBroker, McpSession, NotificationKind, RequestOptions,
    ServerDefinition, StdioDefinition, TransportDefinition, expand_template,
    load_sampling_response, require_trust,
};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = env!("CARGO_BIN_EXE_rustman-mcp-fixture");

/// Generous: process start-up on a loaded CI machine.
const PATIENCE: Duration = Duration::from_secs(30);

struct Fixture {
    session: McpSession,
    dir: tempfile::TempDir,
}

async fn start(args: &[&str]) -> Fixture {
    start_with(args, ConnectOptions::default()).await
}

async fn start_with(args: &[&str], options: ConnectOptions) -> Fixture {
    start_full(args, None, options).await
}

async fn start_full(args: &[&str], timeout_ms: Option<u64>, options: ConnectOptions) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let definition = ServerDefinition {
        name: "fixture".into(),
        transport: TransportDefinition::Stdio(StdioDefinition {
            command: FIXTURE.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            ..StdioDefinition::default()
        }),
        roots: None,
        timeout_ms,
        source: PathBuf::from("rustman.toml"),
    };
    let server = definition.resolve(&Resolver::new(), dir.path()).unwrap();
    let mut store =
        TrustStore::load_with_key(dir.path(), TrustKey::in_dir(dir.path().join("keys"))).unwrap();
    let approved = require_trust(&mut store, &server, true).unwrap();
    let session = tokio::time::timeout(PATIENCE, McpSession::connect(&server, &approved, &options))
        .await
        .expect("connected in time")
        .unwrap();
    Fixture { session, dir }
}

fn no_validation() -> CallOptions {
    CallOptions {
        validate: false,
        ..CallOptions::default()
    }
}

async fn call_text(session: &McpSession, tool: &str) -> String {
    let exchange = session
        .call_tool(tool, json!({}), &CallOptions::default())
        .await
        .unwrap();
    exchange.result.text()
}

/// Wait until the server's stderr has a line containing `needle`.
async fn stderr_line(session: &McpSession, needle: &str) -> String {
    let log = session.stderr().unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(line) = log.lines().into_iter().find(|l| l.contains(needle)) {
                return line;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no stderr line with {needle}: {:?}", log.lines()))
}

fn names(tools: &[rmcp::model::Tool]) -> Vec<String> {
    tools.iter().map(|t| t.name.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_are_listed_across_every_page() {
    let fixture = start(&["--pages", "3"]).await;
    let tools = fixture
        .session
        .list_tools(&RequestOptions::default())
        .await
        .unwrap()
        .result;
    let names = names(&tools);
    for name in ["page1-a", "page1-b", "page2-a", "page2-b", "env", "count"] {
        assert!(names.contains(&name.to_owned()), "{name} in {names:?}");
    }
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_stops_after_100_pages() {
    let fixture = start(&["--endless"]).await;
    let err = fixture
        .session
        .list_tools(&RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "mcp: tools/list returned more than 100 pages"
    );
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_list_change_refetches_the_tools() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let before = session
        .list_tools(&RequestOptions::default())
        .await
        .unwrap();
    assert!(!names(&before.result).contains(&"extra".to_owned()));
    let mut changes = session.tool_changes();
    changes.borrow_and_update();
    assert_eq!(call_text(session, "add_tool").await, "added");
    tokio::time::timeout(PATIENCE, async {
        loop {
            changes.changed().await.unwrap();
            if names(&session.cached_tools().unwrap()).contains(&"extra".to_owned()) {
                break;
            }
        }
    })
    .await
    .expect("the tool list was refetched");
    let kinds: Vec<NotificationKind> = session
        .notifications()
        .entries()
        .into_iter()
        .map(|n| n.kind)
        .collect();
    assert!(
        kinds.contains(&NotificationKind::ToolsListChanged),
        "{kinds:?}"
    );
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_required_arguments_block_the_call_unless_overridden() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let err = session
        .call_tool("search", json!({}), &CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "mcp: invalid arguments: /path: required property missing"
    );
    let ClientError::InvalidArguments(violations) = err else {
        panic!("invalid arguments");
    };
    assert_eq!(violations[0].pointer, "/path");

    let sent = session
        .call_tool("search", json!({}), &no_validation())
        .await
        .unwrap();
    assert_eq!(sent.result.text(), "found null");
    assert_eq!(
        sent.result.overridden[0].to_string(),
        "/path: required property missing"
    );
    let valid = session
        .call_tool("search", json!({"path": "a"}), &CallOptions::default())
        .await
        .unwrap();
    assert!(valid.result.overridden.is_empty());
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_errors_and_protocol_errors_differ() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let failed = session
        .call_tool("fail", json!({}), &CallOptions::default())
        .await
        .unwrap()
        .result;
    assert!(failed.is_error);
    assert_eq!(failed.label(), "tool error");
    assert_eq!(failed.text(), "no such file");

    let unknown = session
        .call_tool("nope", json!({}), &CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(unknown.to_string(), "mcp: unknown tool nope");
    let protocol = session
        .call_tool("nope", json!({}), &no_validation())
        .await
        .unwrap_err();
    assert_eq!(protocol.label(), "protocol error -32602");
    assert!(
        protocol
            .to_string()
            .starts_with("mcp: protocol error -32602: "),
        "{protocol}"
    );
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_content_is_checked_against_the_output_schema() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let bad = session
        .call_tool("count", json!({"value": "3"}), &CallOptions::default())
        .await
        .unwrap()
        .result;
    assert_eq!(bad.structured, Some(json!({"count": "3"})));
    let violations: Vec<String> = bad
        .output_violations
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(violations, ["/count: expected integer"]);
    let good = session
        .call_tool("count", json!({"value": 3}), &CallOptions::default())
        .await
        .unwrap()
        .result;
    assert!(good.output_violations.is_empty());
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_templates_reads_and_subscriptions() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let options = RequestOptions::default();
    let resources = session.list_resources(&options).await.unwrap().result;
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(uris, ["file:///README.md", "file:///logo.png"]);

    let templates = session
        .list_resource_templates(&options)
        .await
        .unwrap()
        .result;
    assert_eq!(templates[0].uri_template, "file:///{path}");
    let params = BTreeMap::from([("path".to_owned(), "README.md".to_owned())]);
    let uri = expand_template(&templates[0].uri_template, &params).unwrap();
    assert_eq!(uri, "file:///README.md");
    let read = session.read_resource(&uri, &options).await.unwrap().result;
    assert_eq!(read[0].text.as_deref(), Some("# Readme"));
    assert_eq!(read[0].mime_type.as_deref(), Some("text/markdown"));
    let blob = session
        .read_resource("file:///logo.png", &options)
        .await
        .unwrap()
        .result;
    assert_eq!(blob[0].blob.as_deref(), Some("iVBORw0KGgo="));

    let mut stream = session.notifications().subscribe();
    session.subscribe(&uri, &options).await.unwrap();
    let updated = tokio::time::timeout(PATIENCE, async {
        loop {
            let note = stream.recv().await.unwrap();
            if let Some(uri) = note.reread_uri() {
                return uri.to_owned();
            }
        }
    })
    .await
    .expect("the update arrived");
    assert_eq!(updated, "file:///README.md");
    session.unsubscribe(&uri, &options).await.unwrap();
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_and_completion() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let options = RequestOptions::default();
    let prompts = session.list_prompts(&options).await.unwrap().result;
    let review = prompts.iter().find(|p| p.name == "review").unwrap();
    let missing = session
        .get_prompt("review", &BTreeMap::new(), Some(review), &options)
        .await
        .unwrap_err();
    assert_eq!(
        missing.to_string(),
        "mcp: invalid arguments: /code: required property missing"
    );
    let arguments = BTreeMap::from([("code".to_owned(), "fn main() {}".to_owned())]);
    let result = session
        .get_prompt("review", &arguments, Some(review), &options)
        .await
        .unwrap()
        .result;
    let roles: Vec<&str> = result.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(result.messages[0].content.summary(), "Review: fn main() {}");

    let target = CompletionTarget::Prompt("greet".into());
    let values = session
        .complete(&target, "language", "ru", &options)
        .await
        .unwrap();
    assert_eq!(values, ["rust", "ruby"]);
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undeclared_capabilities_disable_operations() {
    let fixture = start(&["--no-prompts"]).await;
    let err = fixture
        .session
        .list_prompts(&RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "mcp: server does not declare prompts");
    fixture.session.shutdown().await;
}

fn logs(exchange_notes: &[rustman_mcp::client::Notification]) -> Vec<String> {
    exchange_notes
        .iter()
        .filter_map(|n| match &n.kind {
            NotificationKind::Log { level, .. } => Some(level.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn debug_logs_appear_after_setting_debug() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let before = session
        .call_tool("log", json!({}), &CallOptions::default())
        .await
        .unwrap();
    assert_eq!(logs(&before.notifications), ["info"]);
    session
        .set_log_level(LoggingLevel::Debug, &RequestOptions::default())
        .await
        .unwrap();
    let after = session
        .call_tool("log", json!({}), &CallOptions::default())
        .await
        .unwrap();
    assert_eq!(logs(&after.notifications), ["debug", "info"]);
    let summaries: Vec<String> = session
        .notifications()
        .entries()
        .iter()
        .map(|n| n.summary())
        .collect();
    assert!(
        summaries.contains(&"log debug [fixture] a debug message".to_owned()),
        "{summaries:?}"
    );
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_notifications_are_collected_with_the_call() {
    let fixture = start(&[]).await;
    let exchange = fixture
        .session
        .call_tool("progress5", json!({}), &CallOptions::default())
        .await
        .unwrap();
    let progress = exchange
        .notifications
        .iter()
        .filter(|n| matches!(n.kind, NotificationKind::Progress { .. }))
        .count();
    assert_eq!(progress, 5);
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timeout_cancels_the_request() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let options = CallOptions {
        request: RequestOptions {
            timeout: Some(Duration::from_millis(300)),
            ..RequestOptions::default()
        },
        validate: true,
    };
    let err = session
        .call_tool("hang", json!({}), &options)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "mcp: request timed out after 300ms");
    let line = stderr_line(session, "cancelled").await;
    assert!(line.ends_with("request timed out"), "{line}");
    // The session survives a timed-out request.
    session.ping().await.unwrap();
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_definition_sets_the_timeout() {
    let fixture = start_full(&[], Some(250), ConnectOptions::default()).await;
    let err = fixture
        .session
        .call_tool("hang", json!({}), &CallOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "mcp: request timed out after 250ms");
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_user_cancels_a_call_in_progress() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let cancel = CancellationToken::new();
    let (progress, mut seen) = watch::channel(None);
    let options = CallOptions {
        request: RequestOptions {
            cancel: Some(cancel.clone()),
            progress: Some(progress),
            ..RequestOptions::default()
        },
        validate: true,
    };
    let call = session.call_tool("slow", json!({}), &options);
    let watcher = async {
        let shown = seen
            .wait_for(|p| p.as_ref().and_then(|p| p.percent()) == Some(40.0))
            .await
            .unwrap()
            .clone();
        cancel.cancel();
        shown
    };
    let (result, shown) = tokio::time::timeout(PATIENCE, async { tokio::join!(call, watcher) })
        .await
        .expect("cancelled in time");
    assert_eq!(shown.unwrap().percent(), Some(40.0));
    let err = result.unwrap_err();
    assert!(matches!(err, ClientError::Cancelled));
    assert_eq!(err.to_string(), "cancelled by user");
    let line = stderr_line(session, "cancelled").await;
    assert!(line.ends_with("cancelled by user"), "{line}");
    fixture.session.shutdown().await;
}

fn collecting_broker() -> (HeadlessBroker, Arc<Mutex<Vec<String>>>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let seen = warnings.clone();
    let broker = HeadlessBroker::new(move |w| seen.lock().unwrap().push(w.to_owned()));
    (broker, warnings)
}

fn with_broker(broker: impl ApprovalBroker) -> ConnectOptions {
    ConnectOptions {
        broker: Arc::new(broker),
        ..ConnectOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_sampling_is_rejected_and_reported() {
    let (broker, warnings) = collecting_broker();
    let fixture = start_with(&[], with_broker(broker)).await;
    let reply = call_text(&fixture.session, "sample").await;
    let error: Value = serde_json::from_str(reply.trim_start_matches("error: ")).unwrap();
    assert_eq!(error["code"], -1);
    assert_eq!(error["message"], "User rejected sampling request");
    let warnings = warnings.lock().unwrap().clone();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].starts_with("mcp: rejected sampling request from fixture"),
        "{warnings:?}"
    );
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_canned_sampling_response_answers_headless() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sampling.json");
    std::fs::write(
        &file,
        r#"{"role": "assistant", "content": {"type": "text", "text": "canned"}, "model": "human"}"#,
    )
    .unwrap();
    let broker =
        HeadlessBroker::default().with_sampling_response(load_sampling_response(&file).unwrap());
    let fixture = start_with(&[], with_broker(broker)).await;
    let reply: Value = serde_json::from_str(&call_text(&fixture.session, "sample").await).unwrap();
    assert_eq!(reply["content"]["text"], "canned");
    assert_eq!(reply["model"], "human");
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_sampling_waits_for_the_user() {
    let (broker, mut queue) = InteractiveBroker::new();
    let fixture = start_with(&[], with_broker(broker.clone())).await;
    let session = &fixture.session;
    let options = CallOptions::default();
    let call = session.call_tool("sample", json!({}), &options);
    let user = async {
        let Some(Approval::Sampling { request, reply }) = queue.recv().await else {
            panic!("a sampling approval");
        };
        assert_eq!(request.params.system_prompt.as_deref(), Some("Be brief."));
        assert_eq!(request.params.max_tokens, 100);
        assert_eq!(broker.pending(), 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        reply.send(rustman_mcp::client::SamplingDecision::Reject);
    };
    let (result, ()) = tokio::time::timeout(PATIENCE, async { tokio::join!(call, user) })
        .await
        .expect("answered in time");
    assert!(
        result
            .unwrap()
            .result
            .text()
            .contains("User rejected sampling request")
    );
    assert_eq!(broker.pending(), 0);
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_accepted_elicitation_returns_the_validated_content() {
    let (broker, mut queue) = InteractiveBroker::new();
    let fixture = start_with(&[], with_broker(broker)).await;
    let options = CallOptions::default();
    let call = fixture.session.call_tool("ask_email", json!({}), &options);
    let user = async {
        let Some(Approval::Elicitation { request, reply }) = queue.recv().await else {
            panic!("an elicitation approval");
        };
        assert_eq!(request.message, "Your email?");
        assert!(request.accept(json!({"email": "not-an-email"})).is_err());
        reply.send(request.accept(json!({"email": "dev@example.com"})).unwrap());
    };
    let (result, ()) = tokio::time::timeout(PATIENCE, async { tokio::join!(call, user) })
        .await
        .expect("answered in time");
    let reply: Value = serde_json::from_str(&result.unwrap().result.text()).unwrap();
    assert_eq!(reply["action"], "accept");
    assert_eq!(reply["content"], json!({"email": "dev@example.com"}));
    fixture.session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_elicitation_is_declined() {
    let (broker, warnings) = collecting_broker();
    let fixture = start_with(&[], with_broker(broker)).await;
    let reply: Value =
        serde_json::from_str(&call_text(&fixture.session, "ask_email").await).unwrap();
    assert_eq!(reply["action"], "decline");
    assert_eq!(
        warnings.lock().unwrap().clone(),
        ["mcp: declined elicitation request from fixture: Your email?"]
    );
    fixture.session.shutdown().await;
}

fn root_uri(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_root_is_the_workspace() {
    let fixture = start(&[]).await;
    let session = &fixture.session;
    let reply: Value = serde_json::from_str(&call_text(session, "roots").await).unwrap();
    let roots = reply["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["uri"], root_uri(fixture.dir.path()));

    session
        .set_roots(&["{{workspace}}/docs".to_owned()])
        .await
        .unwrap();
    stderr_line(session, "roots changed").await;
    let reply: Value = serde_json::from_str(&call_text(session, "roots").await).unwrap();
    assert_eq!(
        reply["roots"][0]["uri"],
        root_uri(&fixture.dir.path().join("docs"))
    );
    fixture.session.shutdown().await;
}
