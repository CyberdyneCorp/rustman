//! MQTT publish and subscribe against the in-process broker
//! (add-messaging-client tasks 3.1 and 3.2): QoS 2 completion, wildcard
//! subscriptions, clean sessions with generated client ids, the
//! persistent-session warning, logins from secrets, mutual TLS and the
//! WebSocket transport, for MQTT 3.1.1 and 5.

mod common;

use std::time::Duration;

use common::{Fixture, KAFKA_PW};
use rustman_core::vars::ResolvedMessaging;
use rustman_messaging::{MessagingError, MessagingExchange, RunOptions, run};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::mqtt_broker::{Mode, MqttBroker};
use serde_json::json;
use tokio::sync::oneshot;

const VERSIONS: [&str; 2] = ["\"3.1.1\"", "5"];

fn request(broker: &str, body: &str) -> String {
    format!("version: 1\nkind: mqtt\nname: m\nbroker: '{broker}'\n{body}")
}

async fn exchange(resolved: &ResolvedMessaging) -> Result<MessagingExchange, MessagingError> {
    run(resolved, RunOptions::headless(resolved)).await
}

/// Start a consume of `resolved`, waiting until it is subscribed.
async fn subscribed(
    resolved: ResolvedMessaging,
) -> tokio::task::JoinHandle<Result<MessagingExchange, MessagingError>> {
    let (ready, subscribed) = oneshot::channel();
    let mut options = RunOptions::headless(&resolved);
    options.ready = Some(ready);
    let task = tokio::spawn(async move { run(&resolved, options).await });
    subscribed.await.expect("the consume subscribes");
    task
}

#[tokio::test]
async fn a_qos_2_publish_completes_the_pubrec_pubrel_pubcomp_flow() {
    for version in VERSIONS {
        let broker = MqttBroker::start(Mode::Tcp, None).await;
        let fixture = Fixture::new();
        let resolved = fixture.resolve(
            &request(
                &broker.url(),
                &format!("operation: produce\ntopic: sensors/room1/temp\nprotocol_version: {version}\nqos: 2\nmessages: [{{value: {{c: 21.5}}}}, {{value: '22'}}]\n"),
            ),
            "",
            KAFKA_PW,
        );
        let exchange = exchange(&resolved).await.unwrap();
        let acks: Vec<_> = exchange.produced.iter().map(|m| m.ack.as_deref()).collect();
        assert_eq!(acks, [Some("PUBREC/PUBREL/PUBCOMP"); 2], "{version}");
        assert_eq!(exchange.produced[0].value, Some(json!({"c": 21.5})));
        let log = broker.log();
        assert!(
            log.contains(&"PUBLISH sensors/room1/temp qos2 retain=false {\"c\":21.5}".to_owned()),
            "{log:?}"
        );
        assert!(log.contains(&"PUBREL 1".to_owned()), "{log:?}");
        assert!(log.contains(&"PUBREL 2".to_owned()), "{log:?}");
    }
}

#[tokio::test]
async fn qos_0_and_1_publishes_record_their_acknowledgement() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let fixture = Fixture::new();
    for (qos, ack) in [(0, None), (1, Some("PUBACK"))] {
        let resolved = fixture.resolve(
            &request(&broker.url(), &format!("operation: produce\ntopic: t/1\nqos: {qos}\nretain: true\nmessages: [{{value: x}}]\n")),
            "",
            KAFKA_PW,
        );
        let exchange = exchange(&resolved).await.unwrap();
        assert_eq!(exchange.produced[0].ack.as_deref(), ack);
    }
    assert!(
        broker
            .log()
            .contains(&"PUBLISH t/1 qos1 retain=true x".to_owned())
    );
}

#[tokio::test]
async fn a_wildcard_subscription_records_each_exact_topic() {
    for version in VERSIONS {
        let broker = MqttBroker::start(Mode::Tcp, None).await;
        let fixture = Fixture::new();
        let consume = fixture.resolve(
            &request(&broker.url(), &format!("operation: consume\ntopic: sensors/+/temp\nprotocol_version: {version}\nqos: 2\nlimits: {{max_messages: 2, duration_ms: 20000}}\n")),
            "",
            KAFKA_PW,
        );
        let task = subscribed(consume).await;
        for (topic, value) in [
            ("sensors/room1/humidity", "40"),
            ("sensors/room1/temp", "21"),
            ("sensors/room2/temp", "19"),
        ] {
            let produce = fixture.resolve(
                &request(&broker.url(), &format!("operation: produce\ntopic: {topic}\nprotocol_version: {version}\nmessages: [{{value: '{value}'}}]\n")),
                "",
                KAFKA_PW,
            );
            exchange(&produce).await.unwrap();
        }
        let consumed = task.await.unwrap().unwrap();
        let topics: Vec<_> = consumed
            .messages
            .iter()
            .map(|m| m.topic.clone().unwrap())
            .collect();
        assert_eq!(
            topics,
            ["sensors/room1/temp", "sensors/room2/temp"],
            "{version}"
        );
        assert_eq!(consumed.messages[0].value, Some(json!(21)));
        assert_eq!(consumed.messages[0].qos, Some(1));
        let log = broker.log();
        assert!(
            log.contains(&"SUBSCRIBE sensors/+/temp qos2".to_owned()),
            "{log:?}"
        );
    }
}

#[tokio::test]
async fn subscribes_use_a_clean_session_and_a_generated_client_id() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(
            &broker.url(),
            "operation: consume\ntopic: t/#\nlimits: {duration_ms: 200}\n",
        ),
        "",
        KAFKA_PW,
    );
    let exchange = exchange(&resolved).await.unwrap();
    assert!(exchange.messages.is_empty());
    assert!(exchange.warnings.is_empty());
    let connect = broker
        .log()
        .into_iter()
        .find(|l| l.starts_with("CONNECT"))
        .unwrap();
    let parts: Vec<&str> = connect.split(' ').collect();
    assert_eq!((parts[1], parts[3]), ("5", "clean=true"));
    assert!(
        parts[2].starts_with("rustman-") && parts[2].len() == 16,
        "{connect}"
    );
}

#[tokio::test]
async fn a_persistent_session_warns_that_queued_messages_are_consumed() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(&broker.url(), "operation: consume\ntopic: t/1\nclient_id: probe-1\nclean_session: false\nlimits: {duration_ms: 200}\n"),
        "",
        KAFKA_PW,
    );
    let exchange = exchange(&resolved).await.unwrap();
    assert_eq!(
        exchange.warnings,
        [
            "mqtt: clean_session is false: queued messages for client id 'probe-1' will be consumed from the broker"
        ]
    );
    assert!(
        broker
            .log()
            .contains(&"CONNECT 5 probe-1 clean=false".to_owned())
    );
}

#[tokio::test]
async fn v5_properties_and_user_properties_reach_the_subscriber() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let fixture = Fixture::new();
    let consume = fixture.resolve(
        &request(
            &broker.url(),
            "operation: consume\ntopic: t/1\nlimits: {max_messages: 1, duration_ms: 20000}\n",
        ),
        "",
        KAFKA_PW,
    );
    let task = subscribed(consume).await;
    let produce = fixture.resolve(
        &request(&broker.url(), "operation: produce\ntopic: t/1\nproperties: {content_type: application/json, response_topic: r/1, message_expiry: 60}\nmessages: [{value: {id: 1}, headers: [{name: source, value: rustman}]}]\n"),
        "",
        KAFKA_PW,
    );
    exchange(&produce).await.unwrap();
    let consumed = task.await.unwrap().unwrap();
    let message = &consumed.messages[0];
    assert_eq!(message.headers[0].name, "source");
    assert_eq!(message.headers[0].value, "rustman");
    assert_eq!(
        message.properties,
        Some(
            json!({"content_type": "application/json", "response_topic": "r/1", "message_expiry": 60})
        )
    );
}

#[tokio::test]
async fn logins_come_from_secrets_and_a_wrong_password_is_an_auth_failure() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", KAFKA_PW);
    let fixture = Fixture::new();
    let text = request(
        &broker.url(),
        "operation: produce\ntopic: t/1\nauth: {username: app, password: '{{kafka_pw}}'}\nmessages: [{value: x}]\n",
    );
    let resolved = fixture.resolve(&text, "", KAFKA_PW);
    let ok = exchange(&resolved).await.unwrap();
    let recorded = ok.to_json(&resolved.redactor()).to_string();
    assert!(!recorded.contains(KAFKA_PW), "{recorded}");
    let resolved = fixture.resolve(&text, "", "wrong-pw-123");
    let err = exchange(&resolved).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "mqtt: authentication failed (username/password)"
    );
    let debug = format!("{resolved:?}");
    assert!(!debug.contains("wrong-pw-123"), "{debug}");
}

#[tokio::test]
async fn mutual_tls_presents_the_client_certificate() {
    let certs = TestCerts::generate();
    let broker = MqttBroker::start(Mode::MutualTls, Some(&certs)).await;
    let fixture = Fixture::new();
    fixture.write("tls/ca.pem", &certs.ca_pem);
    fixture.write("tls/client.pem", &certs.client_cert_pem);
    fixture.write("tls/client.key", &certs.client_key_pem);
    let with_cert = request(
        &broker.url(),
        "operation: produce\ntopic: t/1\nqos: 1\ntls: {ca_bundle: tls/ca.pem, client_cert: tls/client.pem, client_key: tls/client.key}\nmessages: [{value: x}]\n",
    );
    let resolved = fixture.resolve(&with_cert, "", KAFKA_PW);
    let exchange_ok = exchange(&resolved).await.unwrap();
    assert_eq!(exchange_ok.produced[0].ack.as_deref(), Some("PUBACK"));
    let without = request(
        &broker.url(),
        "operation: produce\ntopic: t/1\ntls: {ca_bundle: tls/ca.pem}\nmessages: [{value: x}]\n",
    );
    let resolved = fixture.resolve(&without, "", KAFKA_PW);
    let err = exchange(&resolved).await.unwrap_err();
    assert!(err.to_string().starts_with("mqtt: "), "{err}");
    let unknown_ca = request(
        &broker.url(),
        "operation: produce\ntopic: t/1\nmessages: [{value: x}]\n",
    );
    let resolved = fixture.resolve(&unknown_ca, "", KAFKA_PW);
    let err = exchange(&resolved).await.unwrap_err();
    assert!(err.to_string().contains("TLS handshake"), "{err}");
}

#[tokio::test]
async fn the_websocket_transport_carries_mqtt() {
    let broker = MqttBroker::start(Mode::Ws, None).await;
    let fixture = Fixture::new();
    let consume = fixture.resolve(
        &request(
            &broker.url(),
            "operation: consume\ntopic: t/#\nlimits: {max_messages: 1, duration_ms: 20000}\n",
        ),
        "",
        KAFKA_PW,
    );
    let task = subscribed(consume).await;
    let produce = fixture.resolve(
        &request(
            &broker.url(),
            "operation: produce\ntopic: t/ws\nqos: 2\nmessages: [{value: over-ws}]\n",
        ),
        "",
        KAFKA_PW,
    );
    let produced = exchange(&produce).await.unwrap();
    assert_eq!(
        produced.produced[0].ack.as_deref(),
        Some("PUBREC/PUBREL/PUBCOMP")
    );
    let consumed = task.await.unwrap().unwrap();
    assert_eq!(consumed.messages[0].value, Some(json!("over-ws")));
}

#[tokio::test]
async fn an_unreachable_broker_fails_within_the_connect_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let fixture = Fixture::new();
    let resolved = fixture.resolve(
        &request(
            &format!("mqtt://127.0.0.1:{port}"),
            "operation: consume\ntopic: t\n",
        ),
        "",
        KAFKA_PW,
    );
    let mut options = RunOptions::headless(&resolved);
    options.connect_timeout = Duration::from_secs(5);
    let err = run(&resolved, options).await.unwrap_err();
    assert!(err.to_string().starts_with("mqtt: cannot connect"), "{err}");
}
