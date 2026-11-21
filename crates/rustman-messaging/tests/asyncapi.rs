//! AsyncAPI documents (add-messaging-client tasks 4.1-4.3): both versions
//! normalized into one model, the unsupported-version error, the import
//! plan, and payload validation before produce and after consume.

mod common;

use std::path::{Path, PathBuf};

use common::{Fixture, KAFKA_PW};
use rustman_core::assertions::Outcome;
use rustman_core::model::{KindRegistry, MessagingKind, Operation, Request};
use rustman_messaging::asyncapi::{self, AsyncApiError, Document, Version};
use rustman_messaging::{RunOptions, run};
use rustman_test_support::mqtt_broker::{Mode, MqttBroker};
use serde_json::json;
use tokio::sync::oneshot;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asyncapi")
        .join(name)
}

async fn load(name: &str) -> Result<Document, AsyncApiError> {
    asyncapi::load(
        fixture(name).to_str().unwrap(),
        &asyncapi::LoadOptions::default(),
    )
    .await
}

#[tokio::test]
async fn a_3_0_document_normalizes_servers_operations_and_messages() {
    let doc = load("shop-3.0.yaml").await.unwrap();
    let model = &doc.model;
    assert_eq!(model.version, Version::V3_0);
    assert_eq!(model.title, "Shop Events");
    let servers: Vec<_> = model
        .servers
        .iter()
        .map(|s| (s.name.as_str(), s.host.as_str(), s.kind))
        .collect();
    assert_eq!(
        servers,
        [
            (
                "production",
                "kafka.shop.example:9092",
                Some(MessagingKind::Kafka)
            ),
            ("sensors", "mqtt.shop.example", Some(MessagingKind::Mqtt)),
            ("web", "shop.example", None),
        ]
    );
    let ops: Vec<_> = model
        .operations
        .iter()
        .map(|o| {
            (
                o.id.as_str(),
                o.operation,
                o.channel.as_str(),
                o.address.as_str(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        [
            ("sendOrder", Operation::Produce, "orders", "orders"),
            ("onOrder", Operation::Consume, "orders", "orders"),
            (
                "publishReading",
                Operation::Produce,
                "temperature",
                "sensors/{room}/temp"
            ),
            (
                "watchTemperature",
                Operation::Consume,
                "temperature",
                "sensors/{room}/temp"
            ),
        ]
    );
    let send = &model.operations[0];
    assert_eq!(send.servers, ["production"]);
    assert_eq!(send.messages[0].name, "OrderCreated");
    assert_eq!(
        send.messages[0].example,
        Some(json!({"id": 1, "type": "OrderCreated", "total": 9.5}))
    );
    assert_eq!(model.operations[1].messages[0].name, "OrderCreated");
    assert_eq!(model.operations[2].bindings.mqtt_qos, Some(2));
    assert!(doc.message("Reading").unwrap().payload.is_some());
    assert_eq!(
        model.warnings,
        ["asyncapi: the ws binding at /operations/publishReading is ignored"]
    );
}

#[tokio::test]
async fn a_2_6_document_normalizes_publish_and_subscribe() {
    let doc = load("shop-2.6.yaml").await.unwrap();
    let model = &doc.model;
    assert_eq!(model.version, Version::V2_6);
    assert_eq!(model.servers[0].host, "rabbit.shop.example:5672");
    assert_eq!(model.servers[0].kind, Some(MessagingKind::Amqp));
    let ops: Vec<_> = model
        .operations
        .iter()
        .map(|o| (o.id.as_str(), o.operation))
        .collect();
    assert_eq!(
        ops,
        [
            ("publishOrder", Operation::Produce),
            ("onOrder", Operation::Consume),
            ("subscribe-payments", Operation::Consume),
        ]
    );
    let names: Vec<_> = model.operations[1]
        .messages
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, ["OrderCreated", "OrderCancelled"]);
    assert_eq!(
        model.operations[0].bindings.amqp_exchange.as_deref(),
        Some("orders")
    );
}

#[tokio::test]
async fn other_versions_are_refused_with_exit_code_2() {
    let err = load("old-2.0.yaml").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "asyncapi: unsupported version 2.0 (supported: 2.6, 3.0)"
    );
    assert_eq!(err.exit_code(), 2);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openapi.yaml");
    std::fs::write(&path, "openapi: 3.1.0\n").unwrap();
    let err = asyncapi::load_file(&path).await.unwrap_err();
    assert!(
        err.to_string().contains("is not an AsyncAPI document"),
        "{err}"
    );
}

/// The plan as a tree listing followed by every file but the document.
fn rendered(plan: &asyncapi::ImportPlan) -> String {
    let mut out = format!("collection: {}\n", plan.collection);
    for file in plan.environments.iter().chain(&plan.files) {
        out.push_str(&format!("- {}\n", file.path));
    }
    for file in plan
        .environments
        .iter()
        .chain(&plan.files)
        .filter(|f| f.path != "asyncapi.json")
    {
        out.push_str(&format!("\n=== {}\n{}", file.path, file.text));
    }
    for warning in &plan.warnings {
        out.push_str(&format!("\nwarning: {warning}"));
    }
    out
}

#[tokio::test]
async fn the_import_plan_of_a_3_0_document() {
    let doc = load("shop-3.0.yaml").await.unwrap();
    let plan = asyncapi::plan(&doc);
    insta::assert_snapshot!("import_3_0", rendered(&plan));
}

#[tokio::test]
async fn the_import_plan_of_a_2_6_document() {
    let doc = load("shop-2.6.yaml").await.unwrap();
    let plan = asyncapi::plan(&doc);
    insta::assert_snapshot!("import_2_6", rendered(&plan));
}

#[tokio::test]
async fn imported_files_load_and_send_order_is_a_kafka_produce() {
    let doc = load("shop-3.0.yaml").await.unwrap();
    let plan = asyncapi::plan(&doc);
    let ws = Fixture::new();
    let outcome = asyncapi::write(&plan, ws.root(), &ws.root().join("shop")).unwrap();
    assert_eq!(
        outcome.created.len(),
        plan.environments.len() + plan.files.len()
    );
    let again = asyncapi::write(&plan, ws.root(), &ws.root().join("shop")).unwrap();
    assert!(again.created.is_empty());
    let text = std::fs::read_to_string(ws.root().join("shop/orders/send-order.rman.yaml")).unwrap();
    let request = KindRegistry::default()
        .load(Path::new("send-order.rman.yaml"), &text)
        .unwrap();
    let Request::Messaging(request) = request else {
        panic!("a messaging request")
    };
    assert_eq!(request.messaging_kind(), MessagingKind::Kafka);
    assert_eq!(request.effective_operation(), Operation::Produce);
    let value = request.message_list()[0].value.clone().unwrap();
    assert_eq!(
        value,
        rustman_core::model::Payload::Json(json!({"id": 1, "type": "OrderCreated", "total": 9.5}))
    );
    assert!(ws.root().join("environments/production.yaml").exists());
    let loaded = rustman_core::Workspace::load(ws.root()).unwrap();
    assert_eq!(loaded.requests().len(), plan.requests());
}

#[tokio::test]
async fn imported_literals_never_resolve_variables_or_secrets() {
    let doc = load("hostile-3.0.yaml").await.unwrap();
    let plan = asyncapi::plan(&doc);
    let file = plan
        .files
        .iter()
        .find(|f| f.path == "orders/send-order.rman.yaml")
        .unwrap();
    let text = file.text.replace("../asyncapi.json", "asyncapi.json");
    let ws = Fixture::new();
    ws.write("asyncapi.json", &plan.files[0].text);
    let resolved = ws.resolve(&text.replace("{{brokers}}", "127.0.0.1:1"), "", KAFKA_PW);
    assert_eq!(resolved.topic.as_deref(), Some("orders-{{kafka_pw}}"));
    let Some(rustman_core::vars::ResolvedPayload::Json(value)) = &resolved.messages[0].value else {
        panic!("a JSON payload")
    };
    assert_eq!(
        value,
        &json!({
            "token": "{{kafka_pw}}",
            "{{secret:env/HOME}}": 1,
            "nested": ["{{secret:env/HOME}}"]
        })
    );
    assert!(resolved.connection.secrets.is_empty());
}

const ORDER_REF: &str = "asyncapi: {document: asyncapi.yaml, message: OrderCreated}\n";

#[tokio::test]
async fn a_produce_missing_a_required_property_sends_nothing() {
    let ws = Fixture::new();
    ws.write(
        "asyncapi.yaml",
        &std::fs::read_to_string(fixture("shop-3.0.yaml")).unwrap(),
    );
    ws.write(
        "schemas.yaml",
        &std::fs::read_to_string(fixture("schemas.yaml")).unwrap(),
    );
    let resolved = ws.resolve(
        &format!("version: 1\nkind: kafka\nname: p\noperation: produce\nbrokers: 127.0.0.1:1\ntopic: orders\n{ORDER_REF}messages:\n  - value: {{type: OrderCreated}}\n"),
        "",
        KAFKA_PW,
    );
    let err = run(&resolved, RunOptions::headless(&resolved))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("/: missing required property 'id'"), "{text}");
    assert!(text.contains("nothing was sent"), "{text}");
    assert!(
        text.starts_with("asyncapi: message 1 does not match 'OrderCreated'"),
        "{text}"
    );
}

#[tokio::test]
async fn an_unknown_message_is_an_error() {
    let ws = Fixture::new();
    ws.write(
        "asyncapi.yaml",
        &std::fs::read_to_string(fixture("shop-2.6.yaml")).unwrap(),
    );
    let resolved = ws.resolve(
        "version: 1\nkind: kafka\nname: p\noperation: produce\nbrokers: 127.0.0.1:1\ntopic: orders\nasyncapi: {document: asyncapi.yaml, message: Nope}\nmessages: [{value: {id: 1}}]\n",
        "",
        KAFKA_PW,
    );
    let err = run(&resolved, RunOptions::headless(&resolved))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "asyncapi: no message 'Nope' in the document"
    );
}

#[tokio::test]
async fn consumed_violations_fail_the_implicit_assertion() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let ws = Fixture::new();
    ws.write(
        "asyncapi.yaml",
        &std::fs::read_to_string(fixture("shop-3.0.yaml")).unwrap(),
    );
    ws.write(
        "schemas.yaml",
        &std::fs::read_to_string(fixture("schemas.yaml")).unwrap(),
    );
    let consume = ws.resolve(
        &format!("version: 1\nkind: mqtt\nname: c\noperation: consume\nbroker: '{}'\ntopic: sensors/+/temp\nasyncapi: {{document: asyncapi.yaml, message: Reading}}\nlimits: {{max_messages: 2, duration_ms: 20000}}\n", broker.url()),
        "",
        KAFKA_PW,
    );
    let (ready, subscribed) = oneshot::channel();
    let mut options = RunOptions::headless(&consume);
    options.ready = Some(ready);
    let task = tokio::spawn(async move { run(&consume, options).await });
    subscribed.await.unwrap();
    let produce = ws.resolve(
        &format!("version: 1\nkind: mqtt\nname: p\noperation: produce\nbroker: '{}'\ntopic: sensors/a/temp\nmessages: [{{value: {{celsius: 21.5}}}}, {{value: {{room: a}}}}]\n", broker.url()),
        "",
        KAFKA_PW,
    );
    run(&produce, RunOptions::headless(&produce)).await.unwrap();
    let consumed = task.await.unwrap().unwrap();
    assert!(consumed.messages[0].schema_violations.is_empty());
    assert_eq!(
        consumed.messages[1].schema_violations,
        ["/: missing required property 'celsius'"]
    );
    let result = asyncapi::schema_assertion(&consumed).unwrap();
    assert_eq!(result.kind, "asyncapi_schema");
    assert_eq!(result.outcome, Outcome::Fail);
    assert!(
        result
            .message
            .contains("message 2: /: missing required property 'celsius'"),
        "{}",
        result.message
    );
    let json = consumed.to_json(&rustman_core::Redactor::new());
    assert_eq!(json["asyncapi"], "Reading");
}
