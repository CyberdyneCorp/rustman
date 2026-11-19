//! The `realtime_listen` tool (add-realtime-client "Agent tools", task
//! 5.3), driven through real MCP traffic against the in-process realtime
//! servers: frames refused to a read-only agent, a host outside the
//! allowlist, the 100-message ceiling, and an echoed secret redacted.

use std::path::Path;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::{RealtimeSseServer, RealtimeWsServer, SseReply, WsBehavior};
use serde_json::{Value, json};

const CANARY: &str = "realtime-canary-c41e9b07";

struct OneSecret;

impl Keychain for OneSecret {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        Ok(account.ends_with("/api_token").then(|| CANARY.to_owned()))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), KeychainError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<bool, KeychainError> {
        Ok(false)
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment allows `feed_url` (the agents'
/// allowlist comes from its values) and holds the secret `api_token`,
/// with `[agent]` set to `agent`.
fn workspace(feed_url: &str, agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n\n[agent]\n{agent}\n");
    write(dir.path(), "rustman.toml", &toml);
    let env = format!("variables:\n  feed_url: {feed_url}\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "feeds/echo.rman.yaml",
        "version: 1\nkind: ws\nname: Echo\nurl: '{{feed_url}}'\nsteps:\n  - send: {text: 'token {{api_token}}'}\n  - wait_for: {regex: token}\n  - close: {}\n",
    );
    dir
}

type Client = RunningService<RoleClient, ()>;

async fn connect(root: &Path) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, Some(Box::new(OneSecret)));
    let server = Sessions::new(ws, secrets, local).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn listen(client: &Client, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new("realtime_listen")
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

/// The `{code, message}` of a refused call.
fn refusal(result: &CallToolResult) -> (String, String) {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let error = result.structured_content.clone().unwrap();
    (
        error["code"].as_str().unwrap().to_owned(),
        error["message"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_agent_cannot_send_frames() {
    // Scenario "Read-only agent sending frames".
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let ws = workspace(&server.url("/feed"), "allow_mutations = false");
    let client = connect(ws.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "realtime_listen"));
    let result = listen(
        &client,
        json!({"url": "{{feed_url}}", "frames": ["hello", {"op": "subscribe"}]}),
    )
    .await;
    let (code, message) = refusal(&result);
    assert_eq!(
        format!("{code}: {message}"),
        "blocked: sending frames requires allow_mutations"
    );
    let saved = listen(&client, json!({"path": "feeds/echo"})).await;
    assert_eq!(refusal(&saved).1, "sending frames requires allow_mutations");
    assert!(server.sessions().is_empty(), "no session was opened");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused_and_nothing_is_opened() {
    let allowed = RealtimeWsServer::start(WsBehavior::echo()).await;
    let other = RealtimeWsServer::start(WsBehavior::echo()).await;
    let ws = workspace(&allowed.url("/feed"), "");
    let client = connect(ws.path()).await;
    let result = listen(&client, json!({"url": other.url("/feed")})).await;
    assert_eq!(
        refusal(&result),
        ("blocked".to_owned(), "host not allowed".to_owned())
    );
    assert!(other.sessions().is_empty(), "nothing was opened");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_stops_at_100_messages_whatever_the_agent_asks() {
    let server =
        RealtimeSseServer::start(vec![SseReply::forever("{n}", Duration::from_millis(1))]).await;
    let ws = workspace(&server.url("/events"), "");
    let client = connect(ws.path()).await;
    let result = listen(&client, json!({"url": "{{feed_url}}", "max_messages": 500})).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["protocol"], "sse");
    assert_eq!(structured["outcome"], "passed");
    assert_eq!(structured["messages"].as_array().unwrap().len(), 100);
    assert_eq!(structured["end"], "max_messages reached (100)");
    assert_eq!(structured["messages"][0]["data"], "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_echoed_secret_reaches_the_agent_redacted() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let ws = workspace(
        &server.url("/feed"),
        "allow_mutations = true\nrequire_confirmation = false",
    );
    let client = connect(ws.path()).await;
    let result = listen(&client, json!({"path": "feeds/echo"})).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(server.sessions()[0].texts(), [format!("token {CANARY}")]);
    let structured = result.structured_content.clone().unwrap();
    assert_eq!(structured["protocol"], "ws");
    assert_eq!(
        structured["messages"][0]["data"],
        "token «redacted:api_token»"
    );
    let shown = serde_json::to_string(&result).unwrap();
    assert!(!shown.contains(CANARY), "{shown}");
}
