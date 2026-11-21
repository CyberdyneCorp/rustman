//! AMQP publish and consume against a real RabbitMQ (add-messaging-client
//! tasks 3.3 and 3.4). These tests run only when the CI
//! `messaging-mqtt-amqp` job points `RUSTMAN_AMQP_BROKER` (e.g.
//! `amqp://guest:guest@127.0.0.1:5672/%2f`) at its container; elsewhere
//! they return early, so the Linux, macOS and Windows jobs stay hermetic.
//! The CI job sets `RUSTMAN_REQUIRE_BROKERS=mqtt,amqp`, so a missing
//! variable fails instead.

mod common;

use common::{Fixture, KAFKA_PW};
use lapin::options::{BasicPublishOptions, QueueDeclareOptions};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Connection, ConnectionProperties};
use rustman_core::vars::ResolvedMessaging;
use rustman_messaging::{MessagingError, MessagingExchange, RunOptions, run};
use serde_json::json;
use tokio::sync::oneshot;

fn broker() -> Option<String> {
    rustman_test_support::brokers::env("amqp", "RUSTMAN_AMQP_BROKER")
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("rustman-{prefix}-{nanos}")
}

fn request(broker: &str, body: &str) -> String {
    format!("version: 1\nkind: amqp\nname: a\nbroker: '{broker}'\n{body}")
}

async fn exchange(resolved: &ResolvedMessaging) -> Result<MessagingExchange, MessagingError> {
    run(resolved, RunOptions::headless(resolved)).await
}

/// Declare the durable queue `name` and put `count` messages on it.
async fn fill_queue(broker: &str, name: &str, count: usize) {
    let connection = Connection::connect(broker, ConnectionProperties::default())
        .await
        .unwrap();
    let channel = connection.create_channel().await.unwrap();
    channel
        .queue_declare(
            name.into(),
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await
        .unwrap();
    for i in 0..count {
        channel
            .basic_publish(
                "".into(),
                name.into(),
                BasicPublishOptions::default(),
                format!("m{i}").as_bytes(),
                BasicProperties::default(),
            )
            .await
            .unwrap()
            .await
            .unwrap();
    }
    connection.close(200, "bye".into()).await.unwrap();
}

/// The number of messages on `name`; `None` when the queue is gone.
async fn queue_depth(broker: &str, name: &str) -> Option<u32> {
    let connection = Connection::connect(broker, ConnectionProperties::default())
        .await
        .unwrap();
    let channel = connection.create_channel().await.unwrap();
    let passive = QueueDeclareOptions {
        passive: true,
        ..QueueDeclareOptions::default()
    };
    let queue = channel
        .queue_declare(name.into(), passive, FieldTable::default())
        .await
        .ok()
        .map(|q| q.message_count());
    let _ = connection.close(200, "bye".into()).await;
    queue
}

/// Poll the depth of `name` until it is `expected` or 15 s pass (acks
/// and requeues are asynchronous on the broker); the last depth seen.
async fn settled_depth(broker: &str, name: &str, expected: Option<u32>) -> Option<u32> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let depth = queue_depth(broker, name).await;
        if depth == expected || tokio::time::Instant::now() >= deadline {
            return depth;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn an_unroutable_mandatory_message_fails() {
    let Some(broker) = broker() else {
        return;
    };
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(&broker, &format!("operation: produce\nexchange: amq.direct\nrouting_key: {}\nmandatory: true\nmessages: [{{value: lost}}]\n", unique("nobody"))),
        "",
        KAFKA_PW,
    );
    let err = exchange(&resolved).await.unwrap_err();
    assert_eq!(err.to_string(), "amqp: unroutable");
    assert!(matches!(err, MessagingError::Broker { .. }));
}

#[tokio::test]
async fn a_temporary_queue_receives_matching_messages_and_is_deleted() {
    let Some(broker) = broker() else {
        return;
    };
    let fixture = Fixture::new();
    let key = unique("order");
    let consume = fixture.resolve(
        &request(&broker, &format!("operation: consume\nexchange: amq.topic\nbinding_key: '{key}.*'\nlimits: {{max_messages: 1, duration_ms: 20000}}\n")),
        "",
        KAFKA_PW,
    );
    let (ready, subscribed) = oneshot::channel();
    let mut options = RunOptions::headless(&consume);
    options.ready = Some(ready);
    let task = tokio::spawn(async move { run(&consume, options).await });
    subscribed.await.unwrap();
    for routing_key in [format!("other.{key}"), format!("{key}.created")] {
        let produce = fixture.resolve(
            &request(&broker, &format!("operation: produce\nexchange: amq.topic\nrouting_key: '{routing_key}'\nproperties: {{content_type: application/json}}\nmessages: [{{value: {{type: OrderCreated}}, headers: [{{name: source, value: rustman}}]}}]\n")),
            "",
            KAFKA_PW,
        );
        let produced = exchange(&produce).await.unwrap();
        assert_eq!(produced.produced[0].ack.as_deref(), Some("confirm"));
    }
    let consumed = task.await.unwrap().unwrap();
    assert_eq!(consumed.messages.len(), 1);
    let message = &consumed.messages[0];
    assert_eq!(
        message.routing_key.as_deref(),
        Some(format!("{key}.created").as_str())
    );
    assert_eq!(message.value, Some(json!({"type": "OrderCreated"})));
    assert_eq!(message.headers[0].value, "rustman");
    let queue = consumed.queue.clone().unwrap();
    assert_eq!(
        queue_depth(&broker, &queue).await,
        None,
        "the temporary queue is gone"
    );
}

#[tokio::test]
async fn requeue_leaves_the_messages_in_place() {
    let Some(broker) = broker() else {
        return;
    };
    let queue = unique("payments");
    fill_queue(&broker, &queue, 3).await;
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(&broker, &format!("operation: consume\nqueue: {queue}\nrequeue: true\nlimits: {{max_messages: 3, duration_ms: 20000}}\n")),
        "",
        KAFKA_PW,
    );
    for _ in 0..2 {
        let consumed = exchange(&resolved).await.unwrap();
        let values: Vec<_> = consumed
            .messages
            .iter()
            .map(|m| m.value.clone().unwrap())
            .collect();
        assert_eq!(values, [json!("m0"), json!("m1"), json!("m2")]);
    }
    assert_eq!(settled_depth(&broker, &queue, Some(3)).await, Some(3));
}

#[tokio::test]
async fn a_destructive_consume_removes_only_what_it_records() {
    let Some(broker) = broker() else {
        return;
    };
    for ack in ["auto", "manual"] {
        let queue = unique("drain");
        fill_queue(&broker, &queue, 5).await;
        let fixture = Fixture::new();
        let resolved = fixture.resolve(
            &request(&broker, &format!("operation: consume\nqueue: {queue}\ndestructive: true\nack: {ack}\nlimits: {{max_messages: 2, duration_ms: 20000}}\n")),
            "",
            KAFKA_PW,
        );
        assert_eq!(exchange(&resolved).await.unwrap().messages.len(), 2);
        assert_eq!(
            settled_depth(&broker, &queue, Some(3)).await,
            Some(3),
            "ack: {ack} leaves the messages it did not record"
        );
    }
}

#[tokio::test]
async fn a_wrong_password_is_an_authentication_failure() {
    let Some(broker) = broker() else {
        return;
    };
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(&broker, "operation: produce\nrouting_key: q\nauth: {username: guest, password: '{{kafka_pw}}'}\nmessages: [{value: x}]\n"),
        "",
        "wrong-amqp-pw-77",
    );
    let err = exchange(&resolved).await.unwrap_err();
    assert_eq!(err.to_string(), "amqp: authentication failed (PLAIN)");
}
