//! The gRPC agent tools (add-grpc-client "Agent tools", task 6.3), driven
//! through real MCP traffic against in-process tonic servers: a host
//! outside the allowlist (refused and audited), a redacted result, the
//! browser tools, the refusal of client-streaming and bidi methods, and
//! the mutation rule: a method not marked `NO_SIDE_EFFECTS` needs the
//! agent's mutation permission and the human's confirmation.

use std::path::Path;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_mcp::server::audit::AUDIT_DIR;
use rustman_test_support::grpc::{GrpcServer, Reflection};
use serde_json::{Value, json};

const CANARY: &str = "grpc-canary-5e1c90b7d3";

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

/// A workspace whose `local` environment allows `target` (the agents'
/// allowlist comes from its values) and holds the secret `api_token`.
fn workspace(target: &str) -> tempfile::TempDir {
    workspace_with(target, "")
}

/// [`workspace`] with `agent` appended to `rustman.toml`, and a saved
/// request `grpc/mutate` whose local schema leaves `GetProduct` unmarked
/// (a mutation, for the agent rules).
fn workspace_with(target: &str, agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{agent}");
    write(dir.path(), "rustman.toml", &toml);
    write(
        dir.path(),
        "protos/catalog.proto",
        "syntax = \"proto3\";\npackage shop.v1;\nservice Catalog {\n  rpc GetProduct(GetProductRequest) returns (Product);\n}\nmessage GetProductRequest {\n  string id = 1;\n}\nmessage Product {\n  string id = 1;\n}\n",
    );
    write(
        dir.path(),
        "grpc/mutate.rman.yaml",
        "version: 1\nkind: grpc\nname: Mutate\ntarget: '{{grpc_host}}'\nmethod: shop.v1.Catalog/GetProduct\nmessage: {id: p-1}\nschema:\n  protos: [../protos/catalog.proto]\n",
    );
    let env = format!("variables:\n  grpc_host: '{target}'\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "grpc/get.rman.yaml",
        "version: 1\nkind: grpc\nname: Get\ntarget: '{{grpc_host}}'\nmethod: shop.v1.Catalog/GetProduct\nmessage: {id: p-1}\nmetadata:\n  - name: authorization\n    value: 'Bearer {{api_token}}'\nassertions:\n  - grpc_status: OK\n",
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

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
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

fn audit_lines(root: &Path) -> Vec<Value> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(root.join(AUDIT_DIR)).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        lines.extend(
            text.lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()),
        );
    }
    lines
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused_audited_and_not_reached() {
    // Scenario "Host outside allowlist".
    let allowed = GrpcServer::start(Reflection::V1).await;
    let other = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&allowed.target());
    let client = connect(ws.path()).await;
    let args = json!({
        "target": other.target(),
        "method": "shop.v1.Catalog/GetProduct",
        "message": {"id": "p-1"},
    });
    let result = call(&client, "grpc_call", args).await;
    assert_eq!(
        refusal(&result),
        ("blocked".to_owned(), "host not allowed".to_owned())
    );
    let listed = call(&client, "grpc_list", json!({"target": other.target()})).await;
    assert_eq!(refusal(&listed).1, "host not allowed");
    assert!(other.calls().is_empty(), "nothing reached the other server");
    let lines = audit_lines(ws.path());
    let refused: Vec<&Value> = lines
        .iter()
        .filter(|l| l["tool"] == "grpc_call" || l["tool"] == "grpc_list")
        .collect();
    assert_eq!(refused.len(), 2, "{lines:?}");
    assert!(
        refused.iter().all(|l| l["outcome"] == "blocked"),
        "{refused:?}"
    );
    assert!(refused.iter().all(|l| l["reason"] == "host not allowed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_result_is_redacted() {
    let server = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&server.target());
    let client = connect(ws.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    for name in ["grpc_list", "grpc_describe", "grpc_call"] {
        assert!(tools.iter().any(|t| t.name == name), "{name}");
    }
    // The server echoes the id, so the secret comes back in the response.
    let args = json!({
        "target": "{{grpc_host}}",
        "method": "shop.v1.Catalog/GetProduct",
        "message": {"id": "{{api_token}}"},
        "metadata": {"authorization": "Bearer {{api_token}}"},
    });
    let result = call(&client, "grpc_call", args).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.clone().unwrap();
    assert_eq!(structured["outcome"], "passed", "{structured}");
    assert_eq!(structured["status"]["name"], "OK");
    assert_eq!(structured["messages"][0]["id"], "«redacted:api_token»");
    let received = server.calls();
    assert_eq!(
        received[0].text("authorization").as_deref(),
        Some(format!("Bearer {CANARY}").as_str()),
        "the server got the real token"
    );
    let shown = serde_json::to_string(&result).unwrap();
    assert!(!shown.contains(CANARY), "{shown}");
    let saved = call(&client, "grpc_call", json!({"path": "grpc/get"})).await;
    let structured = saved.structured_content.unwrap();
    assert_eq!(structured["outcome"], "passed", "{structured}");
    assert_eq!(structured["request"], "grpc/get");
    let audit = std::fs::read_dir(ws.path().join(AUDIT_DIR)).unwrap();
    for file in audit {
        let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
        assert!(!text.contains(CANARY), "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn services_and_methods_are_listed_and_described() {
    let server = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&server.target());
    let client = connect(ws.path()).await;
    let listed = call(&client, "grpc_list", json!({"path": "grpc/get"})).await;
    let structured = listed.structured_content.unwrap();
    let services = structured["services"].as_array().unwrap();
    let catalog = services
        .iter()
        .find(|s| s["name"] == "shop.v1.Catalog")
        .unwrap();
    assert_eq!(catalog["methods"][1]["call_type"], "server streaming");
    let described = call(
        &client,
        "grpc_describe",
        json!({"target": "{{grpc_host}}", "symbol": "shop.v1.Catalog/ListProducts"}),
    )
    .await;
    let text = serde_json::to_string(&described.structured_content.unwrap()).unwrap();
    assert!(text.contains("server_streaming"), "{text}");
    assert!(
        text.contains("page_size") || text.contains("pageSize"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_requests_and_bidi_calls_are_refused_before_sending() {
    let server = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&server.target());
    let client = connect(ws.path()).await;
    let streamed = call(
        &client,
        "grpc_call",
        json!({"target": "{{grpc_host}}", "method": "shop.v1.Catalog/ListProducts", "message": {"page_size": 2}}),
    )
    .await;
    let structured = streamed.structured_content.unwrap();
    assert_eq!(structured["messages"].as_array().unwrap().len(), 2);
    let bidi = call(
        &client,
        "grpc_call",
        json!({"target": "{{grpc_host}}", "method": "shop.v1.Catalog/Chat"}),
    )
    .await;
    let (code, message) = refusal(&bidi);
    assert_eq!(code, "blocked");
    assert!(
        message.starts_with("only unary and server-streaming methods are available to agents"),
        "{message}"
    );
    assert!(!server.calls().iter().any(|c| c.method == "Chat"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mutation_is_refused_under_the_default_agent_settings() {
    let server = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&server.target());
    let client = connect(ws.path()).await;
    let listed = call(&client, "grpc_list", json!({"target": "{{grpc_host}}"})).await;
    let structured = listed.structured_content.unwrap();
    let methods = &structured["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "shop.v1.Catalog")
        .unwrap()["methods"];
    assert_eq!(methods[0]["read_only"], true, "{methods}");
    assert_eq!(methods[2]["read_only"], false, "{methods}");
    let result = call(&client, "grpc_call", json!({"path": "grpc/mutate"})).await;
    assert_eq!(
        refusal(&result),
        (
            "blocked".to_owned(),
            "mutations not allowed for agents".to_owned()
        )
    );
    assert!(server.calls().is_empty(), "nothing was sent");
    let lines = audit_lines(ws.path());
    let line = lines.iter().find(|l| l["tool"] == "grpc_call").unwrap();
    assert_eq!(line["outcome"], "blocked", "{line}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mutation_needs_the_human_to_confirm() {
    let server = GrpcServer::start(Reflection::V1).await;
    let ws = workspace_with(&server.target(), "\n[agent]\nallow_mutations = true\n");
    let client = connect(ws.path()).await;
    // The test client cannot ask the human (no MCP elicitation).
    let result = call(&client, "grpc_call", json!({"path": "grpc/mutate"})).await;
    assert_eq!(refusal(&result).0, "confirmation_unavailable");
    assert!(server.calls().is_empty(), "nothing was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mutation_goes_through_when_rustman_toml_allows_it_unattended() {
    let server = GrpcServer::start(Reflection::V1).await;
    let agent = "\n[agent]\nallow_mutations = true\nrequire_confirmation = false\n";
    let ws = workspace_with(&server.target(), agent);
    let client = connect(ws.path()).await;
    let result = call(&client, "grpc_call", json!({"path": "grpc/mutate"})).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["outcome"], "passed", "{structured}");
    assert_eq!(server.calls().len(), 1);
}
