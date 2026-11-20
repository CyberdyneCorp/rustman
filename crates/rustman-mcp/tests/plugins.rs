//! Plugins in agent sessions (plugins spec, "Agents and plugin kinds" and
//! "Plugin network access"; add-plugin-system task 7.11): a request of a
//! plugin kind passes the agent gate like an HTTP request (refused unless
//! `[agent] plugin_kinds` lists it and mutations are allowed, audited with
//! its method and URL, in calls and in background jobs), and a plugin's own
//! network requests pass the agent's allowlist on top of its grants and
//! are audited.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use rustman_core::model::KindRegistry;
use rustman_core::plugins::{
    Extensions, KindOutcome, PluginExchange, PluginKind, PluginPane, PluginVerdict, SignOutput,
    SignRequest,
};
use rustman_core::safety::{Permission, SafetySettings};
use rustman_core::{Redactor, Secret, SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_mcp::server::plugin_net::AgentNetGuard;
use rustman_plugin::grant::Grant;
use rustman_plugin::sandbox::HttpRequest;
use rustman_plugin::sandbox::net::GovernedNetwork;
use rustman_test_support::TestServer;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Agent;

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("plugin-agent", "1.0"),
        )
    }
}

type Client = RunningService<RoleClient, Agent>;

/// A plugin set registering the request kind `soap`, counting executions.
#[derive(Default)]
struct Soap {
    executed: AtomicUsize,
}

impl Extensions for Soap {
    fn generate(&self, _: &str, name: &str, _: &[String]) -> Result<String, String> {
        Err(format!("no generator {name}"))
    }
    fn sign(&self, scheme: &str, _: &SignRequest) -> Result<SignOutput, String> {
        Err(format!("no scheme {scheme}"))
    }
    fn check(&self, function: &str, _: &PluginExchange, _: &str) -> PluginVerdict {
        PluginVerdict::Error(format!("no function {function}"))
    }
    fn kinds(&self) -> Vec<PluginKind> {
        vec![PluginKind {
            name: "soap".to_owned(),
            plugin: "soap-plugin".to_owned(),
            pane: PluginPane::default(),
        }]
    }
    fn validate_kind(&self, _: &str, _: &Value) -> Result<(), String> {
        Ok(())
    }
    fn execute_kind(&self, _: &str, _: &Value) -> Result<KindOutcome, String> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(KindOutcome {
            summary: "POST /soap".to_owned(),
            status: Some(200),
            ..KindOutcome::default()
        })
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace with one `soap` request and `agent` as its `[agent]` table.
fn workspace(agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{agent}");
    write(dir.path(), "rustman.toml", &toml);
    write(
        dir.path(),
        "environments/local.yaml",
        "variables:\n  base_url: http://127.0.0.1:9\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "soap/call.rman.yaml",
        "version: 1\nkind: soap\nname: Call\noperation: GetUser\n",
    );
    dir
}

async fn connect(root: &Path, plugins: Arc<Soap>) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let registry = KindRegistry::default().with_opaque_fallback();
    let ws = Workspace::load_with(root, &registry).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, None);
    let plugins: Arc<dyn Extensions> = plugins;
    let server = Sessions::new(ws, secrets, local)
        .with_plugins(Some(plugins))
        .open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    Agent.serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

fn text(result: &CallToolResult) -> String {
    serde_json::to_string(result).unwrap()
}

/// Every line of the workspace's audit log.
fn audit_lines(root: &Path) -> Vec<Value> {
    let Ok(dir) = std::fs::read_dir(root.join(".rustman/audit")) else {
        return Vec::new();
    };
    dir.flat_map(|entry| {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>()
    })
    .collect()
}

fn line_of<'a>(lines: &'a [Value], tool: &str) -> &'a Value {
    lines
        .iter()
        .find(|l| l["tool"] == tool)
        .unwrap_or_else(|| panic!("no audit line for {tool}: {lines:#?}"))
}

/// Without `[agent] plugin_kinds`, `send_request` of a plugin kind is
/// refused before the plugin runs, and the call's audit line names the
/// kind and `<kind>:<request id>`.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_kind_not_listed_for_agents_is_refused_and_audited() {
    let ws = workspace("[agent]\nallow_mutations = true\nrequire_confirmation = false\n");
    let soap = Arc::new(Soap::default());
    let client = connect(ws.path(), soap.clone()).await;
    let result = call(&client, "send_request", json!({"path": "soap/call"})).await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(
        text(&result).contains("plugin kind 'soap' not allowed for agents"),
        "{result:?}"
    );
    assert_eq!(soap.executed.load(Ordering::SeqCst), 0);
    let lines = audit_lines(ws.path());
    let line = line_of(&lines, "send_request");
    assert_eq!(line["outcome"], "blocked", "{line}");
    assert_eq!(line["method"], "soap", "{line}");
    assert_eq!(line["url"], "soap:soap/call", "{line}");
}

/// Listed, a plugin kind still counts as a mutation.
#[tokio::test(flavor = "multi_thread")]
async fn a_listed_plugin_kind_needs_mutations() {
    let ws = workspace("[agent]\nplugin_kinds = [\"soap\"]\nrequire_confirmation = false\n");
    let soap = Arc::new(Soap::default());
    let client = connect(ws.path(), soap.clone()).await;
    let result = call(&client, "send_request", json!({"path": "soap/call"})).await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(
        text(&result).contains("mutations not allowed for agents"),
        "{result:?}"
    );
    assert_eq!(soap.executed.load(Ordering::SeqCst), 0);
}

/// Listed and allowed, it runs once and is audited with its target.
#[tokio::test(flavor = "multi_thread")]
async fn an_allowed_plugin_kind_runs_and_is_audited() {
    let ws = workspace(
        "[agent]\nplugin_kinds = [\"soap\"]\nallow_mutations = true\nrequire_confirmation = false\n",
    );
    let soap = Arc::new(Soap::default());
    let client = connect(ws.path(), soap.clone()).await;
    let result = call(&client, "send_request", json!({"path": "soap/call"})).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(soap.executed.load(Ordering::SeqCst), 1);
    let lines = audit_lines(ws.path());
    let line = line_of(&lines, "send_request");
    assert_eq!(line["outcome"], "ok", "{line}");
    assert_eq!(line["method"], "soap", "{line}");
    assert_eq!(line["url"], "soap:soap/call", "{line}");
}

/// A background run gives each plugin kind request its own audit line,
/// refused like a direct call.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_job_audits_and_gates_each_plugin_kind_request() {
    let ws = workspace("[agent]\nallow_mutations = true\nrequire_confirmation = false\n");
    let soap = Arc::new(Soap::default());
    let client = connect(ws.path(), soap.clone()).await;
    let started = call(&client, "run_collection", json!({"path": "soap"})).await;
    assert_eq!(started.is_error, Some(false), "{started:?}");
    let job = started.structured_content.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    let line = loop {
        let lines = audit_lines(ws.path());
        let found = lines
            .into_iter()
            .find(|l| l["tool"] == "run_collection" && l["arguments"]["job_id"] == job);
        if let Some(line) = found {
            break line;
        }
        assert!(Instant::now() < deadline, "the job wrote no request line");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(line["outcome"], "blocked", "{line}");
    assert_eq!(line["method"], "soap", "{line}");
    assert_eq!(line["url"], "soap:soap/call", "{line}");
    assert_eq!(soap.executed.load(Ordering::SeqCst), 0);
}

const TOKEN: &str = "tok-9a1d77c3e0b4";

/// The network of a plugin granted `net:127.0.0.1:<port>`, guarded as in
/// an agent session of the workspace at `root`.
fn guarded_network(root: &Path, server: &TestServer) -> GovernedNetwork {
    let registry = KindRegistry::default().with_opaque_fallback();
    let ws = Workspace::load_with(root, &registry).unwrap();
    let grant = Grant::parse(&format!("net:127.0.0.1:{}", server.port())).unwrap();
    let mut redactor = Redactor::new();
    redactor.add(&Secret::new("api_token", TOKEN.to_owned()));
    let network = GovernedNetwork::for_grants(
        &SafetySettings::default(),
        Permission::ReadOnly,
        None,
        [&grant],
        None,
        redactor,
    )
    .unwrap()
    .unwrap();
    network.with_guard(AgentNetGuard::shared(&ws, None))
}

fn get(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".to_owned(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

/// A host the plugin is granted but agents may not reach is refused in an
/// agent session, and the refusal is audited (method, redacted URL).
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_request_outside_the_agent_allowlist_is_refused_and_audited() {
    let server = TestServer::http().await;
    let ws = workspace("[agent]\nallowed_hosts = [\"https://api.example.com\"]\n");
    let network = guarded_network(ws.path(), &server);
    let url = format!("http://127.0.0.1:{}/echo?token={TOKEN}", server.port());
    let refused = tokio::task::spawn_blocking(move || network.fetch("auth-fetch", get(url)))
        .await
        .unwrap();
    let refused = format!("{:?}", refused.unwrap_err());
    assert!(
        refused.contains("PolicyDenied") && refused.contains("host not allowed"),
        "{refused}"
    );
    let lines = audit_lines(ws.path());
    let line = line_of(&lines, "plugin:auth-fetch");
    assert_eq!(line["outcome"], "blocked", "{line}");
    assert_eq!(line["method"], "GET", "{line}");
    let shown = line["url"].as_str().unwrap();
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("«redacted:api_token»"), "{shown}");
}

/// A host both granted and on the agent allowlist is reached, and audited.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_request_inside_the_agent_allowlist_is_sent_and_audited() {
    let server = TestServer::http().await;
    let origin = format!("http://127.0.0.1:{}", server.port());
    let ws = workspace(&format!("[agent]\nallowed_hosts = [\"{origin}\"]\n"));
    let network = guarded_network(ws.path(), &server);
    let url = format!("{origin}/echo");
    let sent = tokio::task::spawn_blocking(move || network.fetch("auth-fetch", get(url)))
        .await
        .unwrap();
    assert_eq!(sent.unwrap().status, 200);
    let lines = audit_lines(ws.path());
    let line = line_of(&lines, "plugin:auth-fetch");
    assert_eq!(line["outcome"], "ok", "{line}");
    assert_eq!(line["method"], "GET", "{line}");
}
