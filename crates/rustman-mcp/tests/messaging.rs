//! Messaging requests run by agents (add-messaging-client task 5.3):
//! a read-only agent's produce is refused with `blocked: produce requires
//! allow_mutations` and audited, a broker outside the allowlist is
//! refused, a Schema Registry outside the allowlist is refused, and none
//! of them reaches the broker.

use std::path::Path;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_mcp::server::audit::AUDIT_DIR;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace with `[agent]` set to `agent` and a Kafka produce to
/// `broker`.
fn workspace(broker: &str, agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n\n[agent]\n{agent}\n");
    write(dir.path(), "rustman.toml", &toml);
    write(
        dir.path(),
        "environments/local.yaml",
        "variables:\n  base_url: http://127.0.0.1:9\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "events/publish.rman.yaml",
        &format!(
            "version: 1\nkind: kafka\nname: Publish\noperation: produce\nbrokers: '{broker}'\ntopic: orders\nmessages: [{{value: {{id: 1}}}}]\n"
        ),
    );
    dir
}

type Client = RunningService<RoleClient, ()>;

async fn connect(root: &Path) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, None);
    let server = Sessions::new(ws, secrets, local).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn send(client: &Client) -> CallToolResult {
    send_path(client, "events/publish").await
}

async fn send_path(client: &Client, path: &str) -> CallToolResult {
    let params = CallToolRequestParams::new("send_request")
        .with_arguments(json!({"path": path}).as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

fn refusal(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let error = result.structured_content.clone().unwrap();
    format!(
        "{}: {}",
        error["code"].as_str().unwrap(),
        error["message"].as_str().unwrap()
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

/// A listener standing in for the broker, and whether anything connected
/// to it within a short wait.
async fn broker() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    (listener, addr)
}

async fn reached(listener: &TcpListener) -> bool {
    tokio::time::timeout(Duration::from_millis(200), listener.accept())
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_agent_cannot_produce_and_the_refusal_is_audited() {
    // Scenario "Read-only agent produces".
    let (listener, addr) = broker().await;
    let ws = workspace(
        &addr,
        &format!("allow_mutations = false\nallowed_hosts = [\"http://{addr}\"]"),
    );
    let client = connect(ws.path()).await;
    let result = send(&client).await;
    assert_eq!(
        refusal(&result),
        "blocked: produce requires allow_mutations"
    );
    assert!(!reached(&listener).await, "nothing reached the broker");
    let lines = audit_lines(ws.path());
    let line = lines
        .iter()
        .find(|l| l["tool"] == "send_request")
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(line["outcome"], "blocked");
    assert_eq!(line["reason"], "produce requires allow_mutations");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_broker_outside_the_allowlist_is_refused() {
    let (listener, addr) = broker().await;
    let ws = workspace(
        &addr,
        "allow_mutations = true\nrequire_confirmation = false",
    );
    let client = connect(ws.path()).await;
    let result = send(&client).await;
    assert_eq!(refusal(&result), "blocked: host not allowed");
    assert!(!reached(&listener).await, "nothing reached the broker");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_schema_registry_outside_the_allowlist_is_refused() {
    let (listener, addr) = broker().await;
    let (registry, registry_addr) = broker().await;
    let ws = workspace(&addr, &format!("allowed_hosts = [\"http://{addr}\"]"));
    write(
        ws.path(),
        "events/watch.rman.yaml",
        &format!(
            "version: 1\nkind: kafka\nname: Watch\noperation: consume\nbrokers: '{addr}'\ntopic: orders\nschema_registry: {{url: 'http://{registry_addr}'}}\nlimits: {{duration_ms: 500}}\n"
        ),
    );
    let client = connect(ws.path()).await;
    let result = send_path(&client, "events/watch").await;
    assert_eq!(refusal(&result), "blocked: host not allowed");
    assert!(!reached(&listener).await, "nothing reached the broker");
    assert!(!reached(&registry).await, "nothing reached the registry");
}

/// An MQTT consume of `broker` saved as `events/watch`.
fn write_watch(root: &Path, broker: &str) {
    write(
        root,
        "events/watch.rman.yaml",
        &format!(
            "version: 1\nkind: mqtt\nname: Watch\noperation: consume\nbroker: '{broker}'\ntopic: t\nauth: {{username: app, password: s3cret-mqtt}}\nlimits: {{duration_ms: 500}}\n"
        ),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backslash_cannot_send_an_allowed_broker_elsewhere() {
    // Regression: the policy read `allowed.example` (after the last `@`)
    // while the WebSocket URL parser connected to the host before the
    // backslash, with the MQTT login.
    let (listener, addr) = broker().await;
    let ws = workspace(&addr, "allowed_hosts = [\"http://allowed.example\"]");
    write_watch(ws.path(), &format!("ws://{addr}\\@allowed.example/mqtt"));
    let client = connect(ws.path()).await;
    let result = send_path(&client, "events/watch").await;
    let refused = refusal(&result);
    assert!(
        refused.starts_with("invalid_input: mqtt: invalid broker"),
        "{refused}"
    );
    assert!(!reached(&listener).await, "nothing reached the listener");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_audit_never_records_broker_credentials() {
    // Regression: a `tcp://` broker had no policy URL, so the raw broker
    // string, password included, became the audited target.
    let (listener, addr) = broker().await;
    let ws = workspace(&addr, "allowed_hosts = [\"http://allowed.example\"]");
    write_watch(ws.path(), &format!("tcp://app:hunter2-pw@{addr}"));
    let client = connect(ws.path()).await;
    let result = send_path(&client, "events/watch").await;
    assert_eq!(refusal(&result), "blocked: host not allowed");
    assert!(!reached(&listener).await, "nothing reached the broker");
    let lines = audit_lines(ws.path());
    let line = lines
        .iter()
        .find(|l| l["tool"] == "send_request")
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(line["url"], format!("http://{addr}"));
    let all = serde_json::to_string(&lines).unwrap();
    assert!(!all.contains("hunter2-pw"), "{all}");
}
