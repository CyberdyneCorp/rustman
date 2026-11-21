//! Messaging requests under the `rustman-http` runner (add-messaging-client
//! tasks 5.1-5.4), against the in-process MQTT broker: a `start:
//! before_next` consume that captures the event an HTTP call causes, the
//! quiet-topic bound, assertions and captures over consumed messages, the
//! agent refusal of produce, and history with 64 KiB previews and secrets
//! redacted.
//!
//! The Kafka variant of the end-to-end scenario runs only when the CI
//! `messaging-kafka` job sets `RUSTMAN_KAFKA_BROKERS`.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Fixture, KAFKA_PW};
use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::safety::Permission;
use rustman_core::vars::ResolvedMessaging;
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_messaging::MessagingRunner;
use rustman_test_support::mqtt_broker::{Mode, MqttBroker};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The broker password, a secret of the `dev` environment.
const MQTT_PW: &str = "mqtt-canary-pw-91c2";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace whose `dev` environment sets `variables` and the secret
/// `mqtt_pw`, holding `files`.
fn workspace(variables: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!("variables:\n{variables}secrets: [mqtt_pw]\n"),
    );
    for (rel, text) in files {
        write(root, rel, text);
    }
    let store = SecretStore::with_keychain(root, None);
    store
        .set("dev", &Secret::new("mqtt_pw", MQTT_PW.to_owned()))
        .unwrap();
    dir
}

async fn run_with(root: &Path, messaging: &MessagingRunner) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        messaging: Some(messaging),
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
    runner.run(&plan(&ws, &[], &[])).await.requests
}

async fn run(root: &Path) -> Vec<RequestRecord> {
    run_with(root, &MessagingRunner::default()).await
}

fn history(root: &Path) -> Vec<Value> {
    let dir = root.join(".rustman/history");
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

fn assert_passed(record: &RequestRecord) {
    assert_eq!(
        record.status,
        RequestStatus::Passed,
        "{}: {:?} {:?}",
        record.id,
        record.error,
        record.assertions
    );
}

/// A fake order service: every HTTP request it gets makes it produce
/// `event` (a resolved produce request), then answer `201 Created`.
async fn order_service(event: ResolvedMessaging) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            read_request(&mut tcp).await;
            let options = rustman_messaging::RunOptions::headless(&event);
            rustman_messaging::run(&event, options).await.unwrap();
            let reply = "HTTP/1.1 201 Created\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            tcp.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    url
}

/// Read one HTTP request: its head and a `content-length` body.
async fn read_request(tcp: &mut tokio::net::TcpStream) {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tcp.read(&mut buf).await.unwrap();
        data.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&data).to_ascii_lowercase();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse::<usize>().unwrap());
            if data.len() >= end + 4 + length {
                return;
            }
        }
        if n == 0 {
            return;
        }
    }
}

fn consume_events(broker: &str, extra: &str) -> String {
    format!(
        "version: 1\nkind: mqtt\nname: Order events\noperation: consume\nbroker: '{broker}'\nauth: {{username: app, password: '{{{{mqtt_pw}}}}'}}\ntopic: orders/+\nstart: before_next\n{extra}"
    )
}

const CREATE_ORDER: &str = "\
version: 1
kind: http
name: Create order
method: POST
url: '{{service}}/orders'
body:
  type: json
  content: {item: book}
assertions:
- status: 201
";

#[tokio::test]
async fn a_before_next_consume_captures_the_event_an_http_post_causes() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let event = Fixture::new().resolve(
        &format!(
            "version: 1\nkind: mqtt\nname: e\noperation: produce\nbroker: '{}'\nauth: {{username: app, password: '{MQTT_PW}'}}\ntopic: orders/created\nmessages:\n- value: {{type: OrderCreated, id: 42}}\n  headers: [{{name: source, value: shop}}]\n",
            broker.url()
        ),
        "",
        KAFKA_PW,
    );
    let service = order_service(event).await;
    let consume = consume_events(
        &broker.url(),
        "limits: {max_messages: 1, duration_ms: 20000}\nassertions:\n- messages: {count_eq: 1}\n- json: {path: '$.messages[0].value.type', equals: OrderCreated}\n- header: {name: source, equals: shop}\ncaptures:\n- {var: order_id, from: json, path: '$.messages[0].value.id'}\n",
    );
    let check = "version: 1\nkind: http\nname: Check\nmethod: GET\nurl: '{{service}}/orders/{{order_id}}'\nassertions:\n- status: 201\n";
    let dir = workspace(
        &format!("  service: '{service}'\n"),
        &[
            ("orders/1-events.rman.yaml", &consume),
            ("orders/2-create.rman.yaml", CREATE_ORDER),
            ("orders/3-check.rman.yaml", check),
        ],
    );
    let records = run(dir.path()).await;
    let ids: Vec<&str> = records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        ["orders/1-events", "orders/2-create", "orders/3-check"]
    );
    records.iter().for_each(assert_passed);
    assert_eq!(records[0].assertions.len(), 3);
    assert_eq!(records[0].captures[0].var, "order_id");
    let messaging = records[0].messaging.as_ref().unwrap();
    assert_eq!(
        (messaging.protocol.as_str(), messaging.operation.as_str()),
        ("mqtt", "consume")
    );
    let exchange = messaging.exchange.as_ref().unwrap();
    assert_eq!(exchange["messages"][0]["topic"], "orders/created");
    assert_eq!(
        exchange["messages"][0]["value"],
        json!({"type": "OrderCreated", "id": 42})
    );
}

#[tokio::test]
async fn messages_assertions_fail_when_the_count_differs() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let consume = consume_events(
        &broker.url(),
        "limits: {max_messages: 5, duration_ms: 1000}\nassertions:\n- messages: {count_eq: 1}\n",
    );
    let dir = workspace("  unused: x\n", &[("events.rman.yaml", &consume)]);
    let records = run(dir.path()).await;
    assert_eq!(records[0].status, RequestStatus::Failed);
    let failure = &records[0].assertions[0];
    assert_eq!(failure.label, "messages count == 1");
    assert_eq!(failure.actual.as_deref(), Some("0"));
}

#[tokio::test]
async fn a_quiet_topic_ends_after_its_duration_with_no_messages() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let quiet = format!(
        "version: 1\nkind: mqtt\nname: Quiet\noperation: consume\nbroker: '{}'\nauth: {{username: app, password: '{{{{mqtt_pw}}}}'}}\ntopic: quiet/#\nlimits: {{duration_ms: 2000}}\nassertions:\n- messages: {{count_eq: 0}}\n",
        broker.url()
    );
    let dir = workspace("  unused: x\n", &[("quiet.rman.yaml", &quiet)]);
    let started = Instant::now();
    let records = run(dir.path()).await;
    let elapsed = started.elapsed();
    assert_passed(&records[0]);
    assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(15), "{elapsed:?}");
    let exchange = records[0]
        .messaging
        .as_ref()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap();
    assert_eq!(exchange["messages"], json!([]), "{exchange}");
}

#[tokio::test]
async fn history_keeps_a_64_kib_preview_of_a_200_kib_value_and_no_secret() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let consume = consume_events(
        &broker.url(),
        "limits: {max_messages: 1, duration_ms: 20000}\n",
    );
    let produce = format!(
        "version: 1\nkind: mqtt\nname: Big\noperation: produce\nbroker: '{}'\nauth: {{username: app, password: '{{{{mqtt_pw}}}}'}}\ntopic: orders/big\nmessages:\n- value: {{file: big.txt}}\n  headers: [{{name: token, value: '{{{{mqtt_pw}}}}'}}]\n",
        broker.url()
    );
    let dir = workspace(
        "  unused: x\n",
        &[
            ("1-consume.rman.yaml", &consume),
            ("2-produce.rman.yaml", &produce),
        ],
    );
    write(dir.path(), "big.txt", &"z".repeat(200 * 1024));
    let records = run(dir.path()).await;
    records.iter().for_each(assert_passed);
    let entries = history(dir.path());
    assert_eq!(entries.len(), 2);
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains(MQTT_PW), "the password leaked into history");
    assert!(text.contains("«redacted:mqtt_pw»"));
    let consumed = entries
        .iter()
        .find(|e| e["request"] == "1-consume.rman.yaml")
        .unwrap();
    assert_eq!(consumed["record"]["messaging"]["protocol"], "mqtt");
    assert_eq!(consumed["sent"]["method"], "CONSUME");
    let message = &consumed["record"]["messaging"]["exchange"]["messages"][0];
    assert_eq!(message["size"], 204_800);
    assert_eq!(message["truncated"], true);
    assert_eq!(message["value"].as_str().unwrap().len(), 64 * 1024);
    assert!(consumed["replay"].is_null());
}

#[tokio::test]
async fn literal_credentials_never_reach_history() {
    const AUTH_PW: &str = "LITERAL-auth-pw-77";
    const URL_PW: &str = "LITERAL-url-pw-33";
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", AUTH_PW);
    let url = broker
        .url()
        .replace("mqtt://", &format!("mqtt://app:{URL_PW}@"));
    let produce = format!(
        "version: 1\nkind: mqtt\nname: Literal\noperation: produce\nbroker: '{url}'\nauth: {{username: app, password: {AUTH_PW}}}\ntopic: orders/x\nmessages:\n- value: hi\n"
    );
    let dir = workspace("  unused: x\n", &[("literal.rman.yaml", &produce)]);
    let records = run(dir.path()).await;
    assert_passed(&records[0]);
    let text = serde_json::to_string(&history(dir.path())).unwrap();
    assert!(!text.contains(AUTH_PW), "auth.password leaked: {text}");
    assert!(!text.contains(URL_PW), "the URL password leaked: {text}");
    assert!(
        text.contains("mqtt://app:«redacted:password»@127.0.0.1"),
        "{text}"
    );
}

#[tokio::test]
async fn a_read_only_agent_cannot_produce_and_a_human_can() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let produce = format!(
        "version: 1\nkind: mqtt\nname: Publish\noperation: produce\nbroker: '{}'\nauth: {{username: app, password: '{{{{mqtt_pw}}}}'}}\ntopic: t/1\nmessages: [{{value: hi}}]\n",
        broker.url()
    );
    let dir = workspace("  unused: x\n", &[("publish.rman.yaml", &produce)]);
    let agent = MessagingRunner {
        permission: Some(Permission::ReadOnly),
        ..MessagingRunner::default()
    };
    let refused = run_with(dir.path(), &agent).await;
    assert_eq!(refused[0].status, RequestStatus::Errored);
    assert_eq!(
        refused[0].error.as_deref(),
        Some("blocked: produce requires allow_mutations")
    );
    assert!(refused[0].messaging.as_ref().unwrap().blocked);
    assert!(
        !broker.log().iter().any(|l| l.starts_with("PUBLISH")),
        "{:?}",
        broker.log()
    );
    let allowed = MessagingRunner {
        permission: Some(Permission::Mutations),
        ..MessagingRunner::default()
    };
    assert_passed(&run_with(dir.path(), &allowed).await[0]);
    assert_passed(&run(dir.path()).await[0]);
    let published = broker
        .log()
        .iter()
        .filter(|l| l.starts_with("PUBLISH"))
        .count();
    assert_eq!(published, 2);
}

#[tokio::test]
async fn kafka_before_next_consume_captures_the_event_an_http_post_causes() {
    let Some(brokers) = rustman_test_support::brokers::env("kafka", "RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let topic = format!("rustman-e2e-{nanos}");
    let client = rskafka::client::ClientBuilder::new(vec![brokers.clone()])
        .build()
        .await
        .unwrap();
    let controller = client.controller_client().unwrap();
    controller.create_topic(&topic, 1, 1, 10_000).await.unwrap();
    let event = Fixture::new().resolve(
        &format!("version: 1\nkind: kafka\nname: e\noperation: produce\nbrokers: '{brokers}'\ntopic: {topic}\nmessages:\n- {{key: order-1, value: {{type: OrderCreated}}}}\n"),
        "",
        KAFKA_PW,
    );
    let service = order_service(event).await;
    let consume = format!(
        "version: 1\nkind: kafka\nname: Events\noperation: consume\nbrokers: '{brokers}'\ntopic: {topic}\noffset: latest\nstart: before_next\nlimits: {{max_messages: 1, duration_ms: 20000}}\nassertions:\n- messages: {{count_eq: 1}}\n- json: {{path: '$.messages[0].value.type', equals: OrderCreated}}\n"
    );
    let dir = workspace(
        &format!("  service: '{service}'\n"),
        &[
            ("1-events.rman.yaml", &consume),
            ("2-create.rman.yaml", CREATE_ORDER),
        ],
    );
    let records = run(dir.path()).await;
    records.iter().for_each(assert_passed);
    let quiet = format!(
        "version: 1\nkind: kafka\nname: Quiet\noperation: consume\nbrokers: '{brokers}'\ntopic: {topic}\nlimits: {{duration_ms: 2000}}\nassertions:\n- messages: {{count_eq: 0}}\n"
    );
    let dir = workspace("  unused: x\n", &[("quiet.rman.yaml", &quiet)]);
    let started = Instant::now();
    assert_passed(&run(dir.path()).await[0]);
    assert!(started.elapsed() >= Duration::from_secs(2));
}
