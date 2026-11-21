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

async fn call(client: &Client, tool: &str, args: Value) -> String {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(args.as_object().unwrap().clone());
    let result = client.call_tool(params).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    serde_json::to_string(&result).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn list_and_get_request_never_show_literal_broker_passwords() {
    // Regression: only `kind: http` literals were redacted, so messaging
    // request files reached agents with their passwords in clear.
    let ws = workspace("127.0.0.1:9", "");
    let files = [
        (
            "events/kafka.rman.yaml",
            "version: 1\nkind: kafka\nname: K\noperation: consume\nbrokers: 'h:9092'\ntopic: t\nsecurity:\n  protocol: SASL_SSL\n  mechanism: PLAIN\n  username: app\n  password: kafka-LIT-pw-1\nschema_registry:\n  url: 'http://reg:REG-url-pw-5@registry.example'\n  auth: {type: basic, username: r, password: REG-auth-pw-6}\n",
        ),
        (
            "events/mqtt.rman.yaml",
            "version: 1\nkind: mqtt\nname: M\noperation: consume\nbroker: 'mqtts://app:MQTT-url-pw%2F2@broker.example'\nauth: {username: app, password: MQTT-auth-pw-3}\ntopic: t/#\n",
        ),
        (
            "events/amqp.rman.yaml",
            "version: 1\nkind: amqp\nname: A\noperation: consume\nbroker: 'amqp://guest:AMQP/url-pw-4@rabbit.example/'\nexchange: x\n",
        ),
        (
            "events/ws.rman.yaml",
            "version: 1\nkind: mqtt\nname: W\noperation: consume\nbroker: 'wss://app:WS-url-pw-7@broker.example/mqtt'\ntopic: t\n",
        ),
    ];
    for (path, text) in files {
        write(ws.path(), path, text);
    }
    let client = connect(ws.path()).await;
    let mut shown = vec![call(&client, "list_requests", json!({})).await];
    for id in ["events/kafka", "events/mqtt", "events/amqp", "events/ws"] {
        shown.push(call(&client, "get_request", json!({"path": id})).await);
    }
    let all = shown.join("\n");
    for leaked in [
        "kafka-LIT-pw-1",
        "MQTT-url-pw",
        "MQTT-auth-pw-3",
        "AMQP/url-pw-4",
        "url-pw-4",
        "REG-url-pw-5",
        "REG-auth-pw-6",
        "WS-url-pw-7",
    ] {
        assert!(!all.contains(leaked), "{leaked} in {all}");
    }
    assert!(all.contains("rabbit.example"), "{all}");
}
