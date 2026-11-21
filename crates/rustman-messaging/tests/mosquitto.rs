//! MQTT publish and subscribe against a real Mosquitto (add-messaging-client
//! tasks 3.1 and 3.2). These tests run only when the CI
//! `messaging-mqtt-amqp` job sets the variables below; elsewhere they
//! return early, so the Linux, macOS and Windows jobs stay hermetic. The
//! CI job sets `RUSTMAN_REQUIRE_BROKERS=mqtt,amqp`, so a missing variable
//! fails instead.
//!
//! - `RUSTMAN_MQTT_BROKER`: a plain listener, e.g. `mqtt://127.0.0.1:1883`.
//! - `RUSTMAN_MQTTS_BROKER` and `RUSTMAN_MQTT_TLS_DIR`: a listener that
//!   requires a client certificate, and a folder with `ca.pem`,
//!   `client.pem` and `client.key`.

mod common;

use common::{Fixture, KAFKA_PW};
use rustman_core::vars::ResolvedMessaging;
use rustman_messaging::{MessagingError, MessagingExchange, RunOptions, run};
use serde_json::json;
use tokio::sync::oneshot;

fn env(name: &str) -> Option<String> {
    rustman_test_support::brokers::env("mqtt", name)
}

fn request(broker: &str, body: &str) -> String {
    format!("version: 1\nkind: mqtt\nname: m\nbroker: '{broker}'\n{body}")
}

async fn exchange(resolved: &ResolvedMessaging) -> Result<MessagingExchange, MessagingError> {
    run(resolved, RunOptions::headless(resolved)).await
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("rustman/{prefix}/{nanos}")
}

#[tokio::test]
async fn qos_2_completes_and_wildcards_match_on_mosquitto() {
    let Some(broker) = env("RUSTMAN_MQTT_BROKER") else {
        return;
    };
    for version in ["\"3.1.1\"", "5"] {
        let base = unique("sensors");
        let fixture = Fixture::new();
        let consume = fixture.resolve(
            &request(&broker, &format!("operation: consume\ntopic: '{base}/+/temp'\nprotocol_version: {version}\nqos: 2\nlimits: {{max_messages: 2, duration_ms: 20000}}\n")),
            "",
            KAFKA_PW,
        );
        let (ready, subscribed) = oneshot::channel();
        let mut options = RunOptions::headless(&consume);
        options.ready = Some(ready);
        let task = tokio::spawn(async move { run(&consume, options).await });
        subscribed.await.unwrap();
        for room in ["room1", "room2"] {
            let produce = fixture.resolve(
                &request(&broker, &format!("operation: produce\ntopic: '{base}/{room}/temp'\nprotocol_version: {version}\nqos: 2\nmessages: [{{value: {{room: {room}}}}}]\n")),
                "",
                KAFKA_PW,
            );
            let produced = exchange(&produce).await.unwrap();
            assert_eq!(
                produced.produced[0].ack.as_deref(),
                Some("PUBREC/PUBREL/PUBCOMP")
            );
        }
        let consumed = task.await.unwrap().unwrap();
        let topics: Vec<_> = consumed
            .messages
            .iter()
            .map(|m| m.topic.clone().unwrap())
            .collect();
        assert_eq!(
            topics,
            [format!("{base}/room1/temp"), format!("{base}/room2/temp")]
        );
        assert_eq!(consumed.messages[0].value, Some(json!({"room": "room1"})));
        assert_eq!(consumed.messages[0].qos, Some(2));
    }
}

#[tokio::test]
async fn mutual_tls_connects_with_the_client_certificate() {
    let (Some(broker), Some(dir)) = (env("RUSTMAN_MQTTS_BROKER"), env("RUSTMAN_MQTT_TLS_DIR"))
    else {
        return;
    };
    let fixture = Fixture::new();
    for file in ["ca.pem", "client.pem", "client.key"] {
        let text = std::fs::read_to_string(std::path::Path::new(&dir).join(file)).unwrap();
        fixture.write(&format!("tls/{file}"), &text);
    }
    let resolved = fixture.resolve(
        &request(&broker, "operation: produce\ntopic: rustman/tls\nqos: 1\ntls: {ca_bundle: tls/ca.pem, client_cert: tls/client.pem, client_key: tls/client.key}\nmessages: [{value: secure}]\n"),
        "",
        KAFKA_PW,
    );
    let produced = exchange(&resolved).await.unwrap();
    assert_eq!(produced.produced[0].ack.as_deref(), Some("PUBACK"));
    let without = fixture.resolve(
        &request(&broker, "operation: produce\ntopic: rustman/tls\ntls: {ca_bundle: tls/ca.pem}\nmessages: [{value: x}]\n"),
        "",
        KAFKA_PW,
    );
    assert!(exchange(&without).await.is_err());
}
