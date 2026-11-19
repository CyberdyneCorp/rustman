//! The `rpc_call` tool (add-jsonrpc-client "Agent read-only tool", task
//! 7.2), driven through real MCP traffic against in-process JSON-RPC
//! nodes: the allowlist refusal, the read-only method globs, a guard
//! winning over them, subscriptions and the redacted result.

use std::path::Path;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_jsonrpc::MethodGuard;
use rustman_mcp::server::Sessions;
use rustman_test_support::{JsonrpcHttpServer, RecordedRequest, RpcReply};
use serde_json::{Value, json};

const CANARY: &str = "rpc-canary-6d0b2f41a9";

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

/// A node answering `getblockcount` with 7 and `echo` with its params.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let call = hit.json();
    let result = match call["method"].as_str().unwrap_or_default() {
        "getblockcount" => json!(7),
        _ => call["params"].clone(),
    };
    RpcReply::json(&json!({"jsonrpc": "2.0", "id": call["id"], "result": result}))
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment allows `url` (the agents'
/// allowlist comes from its values) and holds the secret `api_token`.
fn workspace(url: &str, read_only: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n\n[jsonrpc]\nagent_read_only_methods = {read_only}\n"
    );
    write(dir.path(), "rustman.toml", &toml);
    let env = format!("variables:\n  rpc_url: {url}\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    let ws_url = url.replacen("http://", "ws://", 1);
    let heads = format!(
        "version: 1\nkind: jsonrpc\nname: Heads\nendpoint: '{ws_url}'\nparams: [newHeads]\nsubscription:\n  subscribe: eth_subscribe\n  unsubscribe: eth_unsubscribe\n  notification: eth_subscription\n"
    );
    write(dir.path(), "chain/heads.rman.yaml", &heads);
    write(
        dir.path(),
        "chain/count.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Count\nendpoint: '{{rpc_url}}'\nmethod: getblockcount\nassertions:\n  - json: {path: $.result, equals: 7}\n",
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

async fn rpc_call(client: &Client, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new("rpc_call")
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
async fn the_tool_is_registered_and_sends_a_read_only_call() {
    let node = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&node.url("/"), r#"["get*", "echo"]"#);
    let client = connect(ws.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "rpc_call"));
    let result = rpc_call(
        &client,
        json!({"endpoint": "{{rpc_url}}", "method": "echo", "params": ["{{api_token}}"]}),
    )
    .await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["outcome"], "passed");
    assert_eq!(
        structured["calls"][0]["result"],
        json!(["«redacted:api_token»"])
    );
    assert_eq!(node.hits()[0].json()["params"], json!([CANARY]));
    let shown = serde_json::to_string(&structured).unwrap();
    assert!(!shown.contains(CANARY), "{shown}");
    let saved = rpc_call(&client, json!({"path": "chain/count"})).await;
    let structured = saved.structured_content.unwrap();
    assert_eq!(structured["outcome"], "passed", "{structured}");
    assert_eq!(structured["calls"][0]["result"], 7);
    assert_eq!(node.hits().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused_and_nothing_is_sent() {
    // Scenario "Host outside allowlist".
    let allowed = JsonrpcHttpServer::start(node).await;
    let other = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&allowed.url("/"), r#"["get*"]"#);
    let client = connect(ws.path()).await;
    let result = rpc_call(
        &client,
        json!({"endpoint": other.url("/"), "method": "getblockcount"}),
    )
    .await;
    assert_eq!(
        refusal(&result),
        ("blocked".to_owned(), "host not allowed".to_owned())
    );
    assert!(other.hits().is_empty() && allowed.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_method_outside_the_read_only_globs_is_refused() {
    // Scenario "Method outside the allowlist".
    let node = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&node.url("/"), r#"["getblock*"]"#);
    let client = connect(ws.path()).await;
    let result = rpc_call(
        &client,
        json!({"endpoint": "{{rpc_url}}", "method": "sendtoaddress", "params": ["bc1q", 1]}),
    )
    .await;
    assert_eq!(
        refusal(&result),
        (
            "blocked".to_owned(),
            "method not marked read-only for agents".to_owned()
        )
    );
    assert!(node.hits().is_empty());
}

/// Refuses `eth_sendRawTransaction`, as `add-evm-toolkit` would.
struct NoRawTransactions;

impl MethodGuard for NoRawTransactions {
    fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
        (method == "eth_sendRawTransaction")
            .then(|| "eth_sendRawTransaction needs --allow-tx".to_owned())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guard_wins_over_the_read_only_globs() {
    // Scenario "Guard wins over the allowlist".
    let guard: Arc<dyn MethodGuard> = Arc::new(NoRawTransactions);
    rustman_jsonrpc::register_guard(guard.clone());
    let node = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&node.url("/"), r#"["eth_*"]"#);
    let client = connect(ws.path()).await;
    let result = rpc_call(
        &client,
        json!({"endpoint": "{{rpc_url}}", "method": "eth_sendRawTransaction", "params": ["0x00"]}),
    )
    .await;
    rustman_jsonrpc::unregister_guard(&guard);
    assert_eq!(
        refusal(&result),
        (
            "blocked".to_owned(),
            "eth_sendRawTransaction needs --allow-tx".to_owned()
        )
    );
    assert!(node.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_and_unknown_presets_are_refused() {
    let node = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&node.url("/"), r#"["eth_*"]"#);
    let client = connect(ws.path()).await;
    let result = rpc_call(&client, json!({"path": "chain/heads"})).await;
    assert_eq!(
        refusal(&result),
        (
            "blocked".to_owned(),
            "subscriptions are not available to agents".to_owned()
        )
    );
    let unknown = workspace(&node.url("/"), r#"["@solana-read"]"#);
    let client = connect(unknown.path()).await;
    let result = rpc_call(
        &client,
        json!({"endpoint": "{{rpc_url}}", "method": "getblockcount"}),
    )
    .await;
    let (code, message) = refusal(&result);
    assert_eq!(code, "invalid_input");
    assert!(
        message.starts_with("unknown method preset '@solana-read'"),
        "{message}"
    );
    let neither = rpc_call(&client, json!({"method": "getblockcount"})).await;
    assert_eq!(refusal(&neither).0, "invalid_input");
    assert!(node.hits().is_empty());
}
