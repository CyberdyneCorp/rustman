//! The read-only EVM agent tools (add-evm-toolkit task 4.4), driven through
//! real MCP traffic against in-process stub nodes: `tools/list` has no
//! sending tool, each tool answers within its schema, log queries are
//! capped at 10,000 blocks before anything is sent, a host outside the
//! allowlist is refused, and a secret endpoint never reaches the agent.

use std::path::Path;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::{JsonrpcHttpServer, RecordedRequest, RpcReply};
use serde_json::{Value, json};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
const CANARY: &str = "evm-agent-canary-51c7e2";
const EVM_TOOLS: [&str; 6] = [
    "evm_call",
    "evm_decode",
    "evm_tx",
    "evm_block",
    "evm_logs_query",
    "evm_ens",
];

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

fn word(hex: &str) -> String {
    format!("{hex:0>64}")
}

/// A mainnet Geth stub with a token holding 250 USDC for anyone, a block
/// and logs; it echoes nothing it was sent.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let call = hit.json();
    let data = call["params"][0]["data"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let result = match call["method"].as_str().unwrap_or_default() {
        "eth_chainId" => json!("0x1"),
        "web3_clientVersion" => json!("Geth/v1.14.11-stable/linux-amd64/go1.23.2"),
        "eth_blockNumber" => json!("0x4e20"),
        "eth_getBlockByNumber" => json!({"number": "0x10", "hash": "0xaa", "transactions": []}),
        "eth_getLogs" => json!([]),
        "eth_getBalance" => json!("0x0de0b6b3a7640000"),
        "eth_call" if data.starts_with("0x313ce567") => json!(format!("0x{}", word("6"))),
        "eth_call" if data.starts_with("0x95d89b41") => {
            json!(format!("0x{}{}{:0<64}", word("20"), word("4"), "55534443"))
        }
        "eth_call" => json!(format!("0x{}", word("ee6b280"))),
        other => {
            return RpcReply::json(&json!({"jsonrpc": "2.0", "id": call["id"],
                "error": {"code": -32601, "message": format!("the method {other} does not exist")}}));
        }
    };
    RpcReply::json(&json!({"jsonrpc": "2.0", "id": call["id"], "result": result}))
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment points `rpc` at `url`, with a
/// saved evm-call that asserts and captures.
fn workspace(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let env = format!("chain_id: 1\nvariables:\n  rpc: {url}\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    let saved = format!(
        "version: 1\nkind: evm-call\nname: Balance\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: balanceOf\nargs: [\"{VITALIK}\"]\nassertions:\n  - evm: {{output: balance, gte: \"250 USDC\"}}\n"
    );
    write(dir.path(), "token/balance.rman.yaml", &saved);
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

async fn ok(client: &Client, tool: &str, arguments: Value) -> Value {
    let result = call(client, tool, arguments).await;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

fn error(result: &CallToolResult) -> (String, String) {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let error = result.structured_content.clone().unwrap();
    (
        error["code"].as_str().unwrap().to_owned(),
        error["message"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_has_the_read_only_evm_tools_and_no_sending_tool() {
    // Scenario "Agent cannot send".
    let ws = workspace("http://127.0.0.1:9");
    let client = connect(ws.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    for name in EVM_TOOLS {
        let tool = tools.iter().find(|t| t.name == name).expect(name);
        let hints = tool.annotations.as_ref().unwrap();
        assert_eq!(hints.read_only_hint, Some(true), "{name}");
        assert_eq!(hints.destructive_hint, Some(false), "{name}");
    }
    let sending = [
        "send_raw",
        "sendraw",
        "sign",
        "raw_tx",
        "signed",
        "private_key",
    ];
    for tool in &tools {
        let name = tool.name.to_lowercase();
        let schema = serde_json::to_string(&tool.input_schema)
            .unwrap()
            .to_lowercase();
        for word in sending {
            assert!(!name.contains(word), "{name}");
            let field = format!("\"{word}");
            assert!(!schema.contains(&field), "{name} takes {word}");
        }
        assert!(!schema.contains("eth_sendrawtransaction"), "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tool_answers_and_results_conform_to_their_schemas() {
    let stub = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&stub.url("/"));
    let client = connect(ws.path()).await;
    let schemas: std::collections::BTreeMap<String, Value> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| {
            (
                t.name.to_string(),
                Value::Object((*t.output_schema.unwrap()).clone()),
            )
        })
        .collect();
    let calls = [
        ("evm_call", json!({"path": "token/balance"})),
        (
            "evm_call",
            json!({"to": USDC, "abi": "erc20", "function": "balanceOf", "args": [VITALIK]}),
        ),
        (
            "evm_decode",
            json!({"abi": "erc20", "data": "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000000000000a"}),
        ),
        ("evm_block", json!({"block": "latest"})),
        (
            "evm_logs_query",
            json!({"from_block": "19990", "addresses": [USDC]}),
        ),
    ];
    let mut results = Vec::new();
    for (tool, arguments) in calls {
        let value = ok(&client, tool, arguments).await;
        let validator = jsonschema::validator_for(&schemas[tool]).unwrap();
        assert!(validator.is_valid(&value), "{tool}: {value}");
        results.push(value);
    }
    assert_eq!(results[0]["outcome"], "passed", "{}", results[0]);
    assert_eq!(
        results[0]["evm"]["result"]["outputs"]["balance"],
        "250000000"
    );
    assert_eq!(results[0]["evm"]["endpoint"]["client"], "geth");
    assert_eq!(results[1]["request"], "ad-hoc");
    assert_eq!(
        results[2]["result"]["function"],
        "transfer(address,uint256)"
    );
    assert_eq!(results[3]["endpoint"]["chain_id"], 1);
    assert_eq!(results[4]["result"]["from_block"], 19990);
    let methods: Vec<String> = stub
        .hits()
        .iter()
        .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        methods.iter().all(|m| !m.starts_with("eth_send")),
        "{methods:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_log_query_over_50000_blocks_is_refused_before_sending() {
    // Scenario "Log range bounded".
    let stub = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&stub.url("/"));
    let client = connect(ws.path()).await;
    let result = call(
        &client,
        "evm_logs_query",
        json!({"from_block": "1000000", "to_block": "1050000"}),
    )
    .await;
    let (code, message) = error(&result);
    assert_eq!(code, "invalid_input");
    assert_eq!(message, "range too large (max 10000 blocks)");
    assert!(stub.hits().is_empty(), "nothing was sent");
    let result = call(&client, "evm_logs_query", json!({"from_block": "earliest"})).await;
    assert_eq!(error(&result).1, "range too large (max 10000 blocks)");
    assert!(
        stub.hits()
            .iter()
            .all(|h| h.json()["method"] != "eth_getLogs")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused() {
    let allowed = JsonrpcHttpServer::start(node).await;
    let other = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&allowed.url("/"));
    let client = connect(ws.path()).await;
    let result = call(&client, "evm_block", json!({"rpc": other.url("/")})).await;
    let (code, _) = error(&result);
    assert_eq!(code, "blocked");
    assert!(other.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ens_lookup_and_secret_references_in_arguments() {
    let stub = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&stub.url("/"));
    let client = connect(ws.path()).await;
    // The stub answers every eth_call with one word, read as the resolver
    // and the address alike.
    let resolved = ok(
        &client,
        "evm_ens",
        json!({"target": "vitalik.eth", "rpc": "{{rpc}}"}),
    )
    .await;
    assert_eq!(
        resolved["result"]["address"],
        "0x000000000000000000000000000000000ee6B280"
    );
    let result = call(
        &client,
        "evm_call",
        json!({"to": USDC, "abi": "erc20", "function": "name", "rpc": "{{secret:vault/x}}"}),
    )
    .await;
    assert_eq!(error(&result).0, "invalid_input");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_secret_endpoint_never_reaches_the_agent() {
    let stub = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&stub.url("/"));
    let client = connect(ws.path()).await;
    let url = format!("{}{{{{api_token}}}}", stub.url("/"));
    let mut seen = String::new();
    for (tool, arguments) in [
        ("evm_block", json!({"rpc": url})),
        (
            "evm_call",
            json!({"rpc": url, "to": USDC, "abi": "erc20", "function": "totalSupply"}),
        ),
        (
            "evm_tx",
            json!({"rpc": url, "hash": format!("0x{}", "11".repeat(32))}),
        ),
    ] {
        let result = call(&client, tool, arguments).await;
        seen.push_str(&serde_json::to_string(&result).unwrap());
    }
    assert!(
        stub.hits().iter().any(|h| h.uri.contains(CANARY)),
        "the secret was used"
    );
    assert!(!seen.contains(CANARY), "{seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_evm_read_preset_allows_reads_and_the_guard_refuses_sends() {
    // Scenario "Preset enables reads only", end to end: the preset and the
    // send guard as the CLI installs them, through a real rpc_call.
    rustman_evm::install();
    let stub = JsonrpcHttpServer::start(node).await;
    let ws = workspace(&stub.url("/"));
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[jsonrpc]\nagent_read_only_methods = [\"@evm-read\"]\n",
    );
    let client = connect(ws.path()).await;
    let read = ok(
        &client,
        "rpc_call",
        json!({"endpoint": "{{rpc}}", "method": "eth_getBalance", "params": [VITALIK, "latest"]}),
    )
    .await;
    assert_eq!(read["calls"][0]["result"], "0x0de0b6b3a7640000", "{read}");
    let sent = call(
        &client,
        "rpc_call",
        json!({"endpoint": "{{rpc}}", "method": "eth_sendRawTransaction", "params": ["0x02f8"]}),
    )
    .await;
    assert_eq!(
        error(&sent),
        (
            "blocked".to_owned(),
            rustman_evm::blocklist::SEND_REFUSED.to_owned()
        )
    );
    let methods: Vec<String> = stub
        .hits()
        .iter()
        .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        methods,
        ["eth_getBalance"],
        "the send never left the machine"
    );
}
