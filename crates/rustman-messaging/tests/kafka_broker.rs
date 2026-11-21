//! Kafka produce and consume against a real broker (add-messaging-client
//! tasks 2.1-2.3). These tests run only when the CI `messaging-kafka` job
//! points them at its Redpanda containers; elsewhere they return early,
//! so the Linux, macOS and Windows test jobs stay hermetic. The CI job
//! sets `RUSTMAN_REQUIRE_BROKERS=kafka`, so a missing variable fails
//! instead.
//!
//! - `RUSTMAN_KAFKA_BROKERS`: a PLAINTEXT broker, e.g. `127.0.0.1:9092`.
//! - `RUSTMAN_KAFKA_SCRAM_BROKERS` and `RUSTMAN_KAFKA_SCRAM_PASSWORD`: a
//!   broker that requires SASL SCRAM-SHA-512 for user `rustman`.
//!
//! The CI job checks the "no consumer group" scenario itself: it lists
//! the broker's groups after these tests and expects none.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use rskafka::chrono::DateTime;
use rskafka::client::ClientBuilder;
use rskafka::client::partition::{Compression, UnknownTopicHandling};
use rskafka::record::Record;
use rustman_messaging::kafka::partition_for_key;
use rustman_messaging::{MessagingExchange, RunOptions, run};
use serde_json::json;

fn env(name: &str) -> Option<String> {
    rustman_test_support::brokers::env("kafka", name)
}

/// A topic name no other test uses.
fn topic(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("rustman-{prefix}-{nanos}")
}

async fn create_topic(brokers: &str, name: &str, partitions: i32) {
    let client = ClientBuilder::new(vec![brokers.to_owned()])
        .build()
        .await
        .unwrap();
    let controller = client.controller_client().unwrap();
    controller
        .create_topic(name, partitions, 1, 10_000)
        .await
        .unwrap();
}

async fn kafka(text: &str) -> MessagingExchange {
    let resolved = common::resolve(text, "");
    run(&resolved, RunOptions::headless(&resolved))
        .await
        .unwrap()
}

#[tokio::test]
async fn produce_records_partition_offsets_and_consume_reads_them_back() {
    let Some(brokers) = env("RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let orders = topic("orders");
    create_topic(&brokers, &orders, 1).await;
    let produced = kafka(&format!(
        "version: 1\nkind: kafka\nname: p\noperation: produce\nbrokers: '{brokers}'\ntopic: {orders}\npartition: 0\nmessages:\n- {{key: order-1, value: {{id: 1, total: 9.5}}, headers: [{{name: source, value: rustman}}]}}\n- {{value: second}}\n"
    ))
    .await;
    let offsets: Vec<_> = produced
        .produced
        .iter()
        .map(|m| (m.partition, m.offset))
        .collect();
    assert_eq!(offsets, [(Some(0), Some(0)), (Some(0), Some(1))]);
    let consumed = kafka(&format!(
        "version: 1\nkind: kafka\nname: c\noperation: consume\nbrokers: '{brokers}'\ntopic: {orders}\noffset: earliest\nlimits: {{max_messages: 2, duration_ms: 20000}}\n"
    ))
    .await;
    assert_eq!(consumed.messages.len(), 2);
    let first = &consumed.messages[0];
    assert_eq!(first.key, Some(json!("order-1")));
    assert_eq!(first.value, Some(json!({"id": 1, "total": 9.5})));
    assert_eq!(
        (
            first.headers[0].name.as_str(),
            first.headers[0].value.as_str()
        ),
        ("source", "rustman")
    );
    assert_eq!(consumed.messages[1].value, Some(json!("second")));
}

#[tokio::test]
async fn keyed_messages_land_on_the_murmur2_partition() {
    let Some(brokers) = env("RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let keyed = topic("keyed");
    create_topic(&brokers, &keyed, 3).await;
    let produced = kafka(&format!(
        "version: 1\nkind: kafka\nname: p\noperation: produce\nbrokers: '{brokers}'\ntopic: {keyed}\nmessages:\n- {{key: foobar, value: 1}}\n- {{key: '21', value: 2}}\n"
    ))
    .await;
    let partitions: Vec<_> = produced.produced.iter().map(|m| m.partition).collect();
    let expected = [b"foobar".as_slice(), b"21"]
        .map(|key| Some(i32::try_from(partition_for_key(key, 3)).unwrap()));
    assert_eq!(partitions, expected);
}

#[tokio::test]
async fn an_unknown_topic_is_named() {
    let Some(brokers) = env("RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let resolved = common::resolve(
        &format!(
            "version: 1\nkind: kafka\nname: p\noperation: produce\nbrokers: '{brokers}'\ntopic: no-such-topic-x\nmessages: [{{value: 1}}]\n"
        ),
        "",
    );
    let err = run(&resolved, RunOptions::headless(&resolved))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "kafka: unknown topic 'no-such-topic-x'");
}

#[tokio::test]
async fn a_timestamp_offset_returns_only_later_messages() {
    let Some(brokers) = env("RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let timed = topic("timed");
    create_topic(&brokers, &timed, 1).await;
    let client = ClientBuilder::new(vec![brokers.clone()])
        .build()
        .await
        .unwrap();
    let partition = client
        .partition_client(timed.clone(), 0, UnknownTopicHandling::Retry)
        .await
        .unwrap();
    let at = |rfc: &str| {
        DateTime::from_timestamp_millis(rfc.parse::<jiff::Timestamp>().unwrap().as_millisecond())
            .unwrap()
    };
    let record = |value: &str, time: &str| Record {
        key: None,
        value: Some(value.as_bytes().to_vec()),
        headers: BTreeMap::new(),
        timestamp: at(time),
    };
    let records = vec![
        record("before", "2026-11-17T08:59:59Z"),
        record("at", "2026-11-17T09:00:00Z"),
        record("after", "2026-11-17T09:30:00Z"),
    ];
    partition
        .produce(records, Compression::NoCompression)
        .await
        .unwrap();
    let consumed = kafka(&format!(
        "version: 1\nkind: kafka\nname: c\noperation: consume\nbrokers: '{brokers}'\ntopic: {timed}\noffset: {{timestamp: '2026-11-17T09:00:00Z'}}\nlimits: {{duration_ms: 3000}}\n"
    ))
    .await;
    let values: Vec<_> = consumed.messages.iter().map(|m| m.value.clone()).collect();
    assert_eq!(values, [Some(json!("at")), Some(json!("after"))]);
}

#[tokio::test]
async fn a_quiet_topic_ends_at_the_duration_with_nothing_collected() {
    let Some(brokers) = env("RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let quiet = topic("quiet");
    create_topic(&brokers, &quiet, 1).await;
    let started = std::time::Instant::now();
    let consumed = kafka(&format!(
        "version: 1\nkind: kafka\nname: c\noperation: consume\nbrokers: '{brokers}'\ntopic: {quiet}\nlimits: {{duration_ms: 2000}}\n"
    ))
    .await;
    assert!(consumed.messages.is_empty());
    assert!(started.elapsed() >= Duration::from_millis(2000));
}

#[tokio::test]
async fn scram_sha_512_authenticates_and_history_hides_the_password() {
    let (Some(brokers), Some(password)) = (
        env("RUSTMAN_KAFKA_SCRAM_BROKERS"),
        env("RUSTMAN_KAFKA_SCRAM_PASSWORD"),
    ) else {
        return;
    };
    let text = format!(
        "version: 1\nkind: kafka\nname: c\noperation: consume\nbrokers: '{brokers}'\ntopic: scram-check\nsecurity: {{protocol: SASL_PLAINTEXT, mechanism: SCRAM-SHA-512, username: rustman, password: '{{{{kafka_pw}}}}'}}\nlimits: {{duration_ms: 1000}}\n"
    );
    let resolved = common::resolve_with(&text, "", &password);
    let exchange = run(&resolved, RunOptions::headless(&resolved))
        .await
        .unwrap();
    let recorded = exchange.to_json(&resolved.redactor()).to_string();
    assert!(recorded.contains("«redacted:kafka_pw»"), "{recorded}");
    assert!(!recorded.contains(&password), "{recorded}");
    let wrong = common::resolve_with(&text, "", "not-the-password");
    let mut options = RunOptions::headless(&wrong);
    options.connect_timeout = Duration::from_secs(3);
    let err = run(&wrong, options).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "kafka: authentication failed (SCRAM-SHA-512)"
    );
}
