//! Agents and provider secrets (secret-providers spec "Agents never receive
//! secret values", add-secret-providers task 5.2): request definitions show
//! the reference text, sent requests resolve it inside the engine and their
//! results are redacted, no tool reads or tests a secret, and references
//! written by the agent (ad-hoc requests, variables) are refused.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{
    DEFAULT_TTL, FetchFuture, Origin, ProviderError, ProviderInfo, SecretProvider, SecretProviders,
};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::TestServer;
use serde_json::{Value, json};

const CANARY: &str = "op-agent-canary-6a1f93c2";
const REFERENCE: &str = "{{secret:op/Eng/API/token}}";

/// A 1Password stand-in that counts its fetches.
#[derive(Default)]
struct FakeOp {
    fetches: AtomicUsize,
}

impl SecretProvider for FakeOp {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            match path {
                "Eng/API/token" => Ok(Secret::new(path, CANARY.to_owned())),
                _ => Err(ProviderError::NotFound(format!(
                    "onepassword: {path} not found"
                ))),
            }
        })
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn workspace(server: &TestServer) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[secret_providers.op]\ntype = \"onepassword\"\n",
    );
    write(
        dir.path(),
        "environments/local.yaml",
        &format!("variables:\n  base_url: {}\n", server.url("")),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "users/echo.rman.yaml",
        &format!(
            "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: \"{{{{base_url}}}}/echo\"\nheaders:\n  - name: Authorization\n    value: \"Bearer {REFERENCE}\"\n  - name: X-Token\n    value: \"{REFERENCE}\"\n"
        ),
    );
    write(
        dir.path(),
        "users/missing.rman.yaml",
        "version: 1\nkind: http\nname: Missing\nmethod: GET\nurl: \"{{base_url}}/echo?k={{secret:op/Eng/API/nope}}\"\n",
    );
    dir
}

type Client = RunningService<RoleClient, ()>;

async fn connect(root: &Path, op: Arc<FakeOp>) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, None);
    let mut providers = SecretProviders::new();
    let info = ProviderInfo {
        name: "op".into(),
        kind: "onepassword".into(),
        origin: Origin::Workspace,
        auth: None,
        timeout: Duration::from_secs(10),
        ttl: DEFAULT_TTL,
    };
    providers.insert(info, op);
    let server = Sessions::new(ws, secrets, local)
        .with_secret_providers(Arc::new(providers))
        .open();
    let (server_io, client_io) = tokio::io::duplex(1 << 22);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &str, arguments: Value) -> String {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    let result: CallToolResult = client.call_tool(params).await.unwrap();
    serde_json::to_string(&result).unwrap()
}

#[tokio::test]
async fn agent_reads_a_request_file() {
    let server = TestServer::http().await;
    let ws = workspace(&server);
    let op = Arc::new(FakeOp::default());
    let client = connect(ws.path(), op.clone()).await;
    let text = call(&client, "get_request", json!({"path": "users/echo"})).await;
    let result: Value = serde_json::from_str(&text).unwrap();
    let file = result["structuredContent"]["text"].as_str().unwrap();
    assert!(
        file.contains(&format!("value: \"Bearer {REFERENCE}\"")),
        "{file}"
    );
    assert!(!text.contains(CANARY));
    assert_eq!(op.fetches.load(Ordering::SeqCst), 0, "reading fetched");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn agent_sends_the_request() {
    let server = TestServer::http().await;
    let ws = workspace(&server);
    let op = Arc::new(FakeOp::default());
    let client = connect(ws.path(), op.clone()).await;
    let sent = call(&client, "send_request", json!({"path": "users/echo"})).await;
    assert!(!sent.contains(CANARY), "{sent}");
    assert!(sent.contains("«redacted:op/Eng/API/token»"), "{sent}");
    assert_eq!(op.fetches.load(Ordering::SeqCst), 1);
    // The request reached the server with the value.
    let history = call(&client, "search_history", json!({})).await;
    assert!(!history.contains(CANARY), "{history}");
    // An unresolved reference is an error naming it, never a value.
    let failed = call(&client, "send_request", json!({"path": "users/missing"})).await;
    assert!(
        failed.contains("unresolved secret op/Eng/API/nope: onepassword: Eng/API/nope not found"),
        "{failed}"
    );
    // Nothing on disk holds the value.
    for entry in walk(ws.path()) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
            "{} holds the value",
            entry.display()
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn agent_written_references_are_refused() {
    let server = TestServer::http().await;
    let ws = workspace(&server);
    let op = Arc::new(FakeOp::default());
    let client = connect(ws.path(), op.clone()).await;
    let url = server.url("/echo");
    let attempts = [
        json!({"url": url, "headers": {"Authorization": format!("Bearer {REFERENCE}")}}),
        json!({"url": format!("{url}?k={{{{ secret:op/Eng/API/token }}}}")}),
        json!({"url": url, "method": "POST", "body": REFERENCE}),
        json!({"path": "users/echo", "vars": {"base_url": REFERENCE}}),
    ];
    for arguments in attempts {
        let refused = call(&client, "send_request", arguments.clone()).await;
        assert!(
            refused.contains(
                "secret references ({{secret:...}}) are allowed only in saved request and environment files"
            ),
            "{arguments}: {refused}"
        );
        assert!(!refused.contains(CANARY), "{refused}");
    }
    assert_eq!(
        op.fetches.load(Ordering::SeqCst),
        0,
        "a refused call fetched"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn no_tool_reads_or_tests_a_secret() {
    let server = TestServer::http().await;
    let ws = workspace(&server);
    let client = connect(ws.path(), Arc::default()).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(!tools.is_empty());
    for tool in &tools {
        let name = tool.name.to_lowercase();
        assert!(!name.contains("secret"), "tool {name} exposes secrets");
    }
    client.cancel().await.unwrap();
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
