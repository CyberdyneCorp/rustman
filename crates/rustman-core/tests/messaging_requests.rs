//! `kind: kafka`, `kind: mqtt` and `kind: amqp` request files
//! (add-messaging-client tasks 1.2, 1.3): loading through the workspace,
//! interpolation of every string field, secrets kept out of `Debug`
//! output, and payload files contained in the workspace.

use std::fs;

use rustman_core::Workspace;
use rustman_core::model::{Acks, KafkaProtocol, MessagingKind, Operation, StartOffset};
use rustman_core::vars::{ResolvedPayload, Resolver, SecretLookup, resolve_messaging};
use serde_json::json;

const PRODUCE: &str = "\
version: 1
kind: kafka
name: Create order
operation: produce
brokers: ['{{kafka_host}}:9092', 'b2:9092,b3:9092']
topic: '{{topic}}'
partition: 0
acks: leader
security:
  protocol: SASL_SSL
  mechanism: SCRAM-SHA-512
  username: app
  password: '{{kafka_pw}}'
tls:
  ca_bundle: certs/ca.pem
schema_registry:
  url: 'http://{{kafka_host}}:8081'
  subject: orders-value
messages:
- key: order-{{order}}
  value: {id: 1, total: 9.5, note: '{{note}}'}
  headers:
  - name: source
    value: rustman
  - name: trace
    value: '{{order}}'
- value: {base64: AAE=}
- value: {file: fixtures/blob.bin}
";

const CONSUME: &str = "\
version: 1
kind: kafka
name: Watch orders
operation: consume
brokers: '{{kafka_host}}:9092'
topic: orders
offset: {timestamp: '{{since}}'}
start: before_next
";

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "dev" && name == "kafka_pw").then(|| "kafka-s3cr3t-pw".to_owned()))
    }
}

fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("environments")).unwrap();
    fs::write(
        root.join("environments/dev.yaml"),
        "variables:\n  kafka_host: k1\n  topic: orders\n  order: '7'\n  note: hi\n  since: '2026-11-17T09:00:00Z'\nsecrets: [kafka_pw]\n",
    )
    .unwrap();
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn a_produce_resolves_every_field_and_hides_the_password() {
    let dir = workspace(&[
        ("orders/create.rman.yaml", PRODUCE),
        ("orders/fixtures/blob.bin", "\x01\x02"),
        ("orders/certs/ca.pem", "pem"),
    ]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Create order")[0];
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    let resolved = resolve_messaging(request, &resolver).unwrap();
    check_produce_connection(&resolved);
    check_produce_messages(&resolved);
    let shown = format!("{resolved:?}");
    assert!(!shown.contains("kafka-s3cr3t-pw"), "{shown}");
    let redacted = resolved
        .redactor()
        .text("password=kafka-s3cr3t-pw")
        .into_owned();
    assert_eq!(redacted, "password=«redacted:kafka_pw»");
}

/// The brokers, destination, security, TLS and registry of `PRODUCE`.
fn check_produce_connection(resolved: &rustman_core::vars::ResolvedMessaging) {
    assert_eq!(resolved.kind, MessagingKind::Kafka);
    assert_eq!(resolved.operation, Operation::Produce);
    assert_eq!(resolved.brokers, ["k1:9092", "b2:9092", "b3:9092"]);
    assert_eq!(resolved.connection.url.text, "k1:9092,b2:9092,b3:9092");
    assert_eq!(resolved.topic.as_deref(), Some("orders"));
    assert_eq!(resolved.destination(), "orders");
    assert_eq!(resolved.acks, Acks::Leader);
    let security = resolved.security.as_ref().unwrap();
    assert_eq!(security.effective_protocol(), KafkaProtocol::SaslSsl);
    assert_eq!(security.password.as_deref(), Some("kafka-s3cr3t-pw"));
    assert!(resolved.tls);
    assert!(
        resolved
            .connection
            .tls
            .ca_bundle
            .as_ref()
            .unwrap()
            .path
            .ends_with("ca.pem")
    );
    let registry = resolved.schema_registry.as_ref().unwrap();
    assert_eq!(registry.http.url.text, "http://k1:8081");
    assert_eq!(registry.subject.as_deref(), Some("orders-value"));
}

/// The text, JSON, base64 and file messages of `PRODUCE`.
fn check_produce_messages(resolved: &rustman_core::vars::ResolvedMessaging) {
    let first = &resolved.messages[0];
    assert_eq!(first.key, Some(ResolvedPayload::Text("order-7".into())));
    assert_eq!(
        first.value,
        Some(ResolvedPayload::Json(
            json!({"id": 1, "total": 9.5, "note": "hi"})
        ))
    );
    assert_eq!(
        first.headers,
        [
            ("source".to_owned(), "rustman".to_owned()),
            ("trace".to_owned(), "7".to_owned())
        ]
    );
    assert_eq!(
        resolved.messages[1].value,
        Some(ResolvedPayload::Base64("AAE=".into()))
    );
    let Some(ResolvedPayload::File(file)) = &resolved.messages[2].value else {
        panic!("a file payload");
    };
    assert!(file.path.ends_with("fixtures/blob.bin"), "{file:?}");
}

#[test]
fn a_consume_resolves_its_timestamp_offset_and_start() {
    let dir = workspace(&[("watch.rman.yaml", CONSUME)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Watch orders")[0];
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    let resolved = resolve_messaging(request, &resolver).unwrap();
    assert_eq!(resolved.operation, Operation::Consume);
    assert_eq!(resolved.brokers, ["k1:9092"]);
    assert_eq!(
        resolved.offset,
        StartOffset::Timestamp("2026-11-17T09:00:00Z".into())
    );
    assert_eq!(resolved.start, rustman_core::model::Start::BeforeNext);
    assert!(resolved.messages.is_empty());
}

#[test]
fn missing_variables_are_reported_together() {
    let dir = workspace(&[("watch.rman.yaml", CONSUME)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Watch orders")[0];
    let resolver = Resolver::for_request(&ws.settings, request, None, &Store);
    let err = resolve_messaging(request, &resolver)
        .unwrap_err()
        .to_string();
    assert!(err.contains("kafka_host") && err.contains("since"), "{err}");
}

#[test]
fn a_payload_file_outside_the_workspace_is_refused() {
    let text = "version: 1\nkind: kafka\nname: Leak\noperation: produce\nbrokers: a:9092\ntopic: t\nmessages:\n- value: {file: '{{p}}'}\nvariables:\n  p: ../../etc/passwd\n";
    let dir = workspace(&[("leak.rman.yaml", text)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Leak")[0];
    let resolver = Resolver::for_request(&ws.settings, request, None, &Store);
    let err = resolve_messaging(request, &resolver).unwrap_err();
    assert!(err.to_string().contains("escapes workspace"), "{err}");
}

#[test]
fn a_literal_payload_path_escape_fails_to_load() {
    let text = "version: 1\nkind: kafka\nname: Leak\noperation: produce\nbrokers: a:9092\ntopic: t\nmessages:\n- value: {file: ../../etc/passwd}\n";
    let dir = workspace(&[("leak.rman.yaml", text)]);
    let err = Workspace::load(dir.path()).unwrap_err().to_string();
    assert!(err.contains("escapes workspace"), "{err}");
}

#[test]
fn other_kinds_are_not_messaging_requests() {
    let text = "version: 1\nkind: http\nname: H\nurl: http://x/\n";
    let dir = workspace(&[("h.rman.yaml", text)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("H")[0];
    let resolver = Resolver::for_request(&ws.settings, request, None, &Store);
    let err = resolve_messaging(request, &resolver).unwrap_err();
    assert!(err.to_string().contains("not a messaging request"), "{err}");
}

#[test]
fn the_complete_examples_in_the_messaging_docs_load() {
    let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/messaging.md");
    let text = fs::read_to_string(doc).unwrap().replace("\r\n", "\n");
    let blocks: Vec<&str> = text
        .split("```yaml\n")
        .skip(1)
        .filter_map(|b| b.split("```").next())
        .filter(|b| b.starts_with("version: 1"))
        .collect();
    assert!(!blocks.is_empty());
    let registry = rustman_core::model::KindRegistry::default();
    for block in blocks {
        let loaded = registry.load(std::path::Path::new("doc.rman.yaml"), block);
        assert!(loaded.is_ok(), "{block}: {loaded:?}");
    }
}
