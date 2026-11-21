use serde_json::json;

use super::*;
use crate::model::KindRegistry;

fn parse(text: &str) -> Result<MessagingRequest, LoadError> {
    MessagingRequest::parse(Path::new("m.rman.yaml"), text)
}

fn kafka(body: &str) -> String {
    format!("version: 1\nkind: kafka\nname: orders\n{body}")
}

#[test]
fn a_kafka_produce_loads_with_its_defaults() {
    let r = parse(&kafka(
        "operation: produce\nbrokers: [\"{{host}}:9092\"]\ntopic: orders\npartition: 0\nmessages:\n  - {key: order-1, value: {id: 1, total: 9.5}, headers: [{name: source, value: rustman}]}\n",
    ))
    .unwrap();
    assert_eq!(r.messaging_kind(), MessagingKind::Kafka);
    assert_eq!(r.effective_operation(), Operation::Produce);
    assert_eq!(r.brokers, Some(Brokers(vec!["{{host}}:9092".into()])));
    assert_eq!(r.partition, Some(0));
    assert_eq!(r.effective_acks(), Acks::All);
    assert_eq!(r.effective_start(), Start::Immediately);
    let messages = r.message_list();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].key, Some(Payload::Text("order-1".into())));
    assert_eq!(
        messages[0].value,
        Some(Payload::Json(json!({"id": 1, "total": 9.5})))
    );
    assert_eq!(
        messages[0].headers.as_deref(),
        Some(
            &[MessageHeader {
                name: "source".into(),
                value: "rustman".into()
            }][..]
        )
    );
    assert_eq!(r.source.lines().next(), Some("version: 1"));
}

#[test]
fn a_kafka_consume_loads_offsets_partitions_limits_and_start() {
    let r = parse(&kafka(
        "operation: consume\nbrokers: \"a:9092,b:9092\"\ntopic: orders\npartitions: [0, 2]\noffset: {timestamp: \"2026-11-17T09:00:00Z\"}\nlimits: {max_messages: 5}\nstart: before_next\n",
    ))
    .unwrap();
    assert_eq!(r.effective_operation(), Operation::Consume);
    assert_eq!(r.partitions, Some(vec![0, 2]));
    assert_eq!(
        r.effective_offset(),
        StartOffset::Timestamp("2026-11-17T09:00:00Z".into())
    );
    let limits = r.effective_limits();
    assert_eq!(limits.effective_max_messages(), 5);
    assert_eq!(limits.effective_duration_ms(), 30_000);
    assert_eq!(limits.effective_max_message_bytes(), 1024 * 1024);
    assert_eq!(r.effective_start(), Start::BeforeNext);
    for (offset, expected) in [
        ("earliest", StartOffset::Earliest),
        ("latest", StartOffset::Latest),
        ("42", StartOffset::Absolute(42)),
    ] {
        let r = parse(&kafka(&format!(
            "operation: consume\nbrokers: a:9092\ntopic: t\noffset: {offset}\n"
        )))
        .unwrap();
        assert_eq!(r.effective_offset(), expected, "{offset}");
    }
    let r = parse(&kafka("operation: consume\nbrokers: a:9092\ntopic: t\n")).unwrap();
    assert_eq!(r.effective_offset(), StartOffset::Latest);
    assert_eq!(r.effective_limits().effective_max_messages(), 10);
}

#[test]
fn mqtt_and_amqp_files_load() {
    let mqtt = parse(
        "version: 1\nkind: mqtt\nname: temp\noperation: consume\nbroker: mqtt://localhost\ntopic: sensors/+/temp\nauth: {username: app, password: \"{{mqtt_pw}}\"}\n",
    )
    .unwrap();
    assert_eq!(mqtt.messaging_kind(), MessagingKind::Mqtt);
    assert_eq!(mqtt.topic.as_deref(), Some("sensors/+/temp"));
    assert!(!format!("{:?}", mqtt.auth).contains("mqtt_pw"));
    let amqp = parse(
        "version: 1\nkind: amqp\nname: pub\noperation: produce\nbroker: amqp://localhost\nexchange: orders\nrouting_key: order.created\nmessages: [{value: hi}]\n",
    )
    .unwrap();
    assert_eq!(amqp.messaging_kind(), MessagingKind::Amqp);
    assert_eq!(amqp.exchange.as_deref(), Some("orders"));
}

#[test]
fn a_missing_or_unknown_operation_is_refused() {
    let err = parse(&kafka("brokers: a:9092\ntopic: t\n")).unwrap_err();
    assert_eq!(
        err.message,
        "messaging: operation must be produce or consume"
    );
    let err = parse(&kafka("operation: publish\nbrokers: a:9092\ntopic: t\n")).unwrap_err();
    assert_eq!(err.message, OPERATION_ERROR);
    assert_eq!(err.position.map(|p| p.line), Some(4));
}

#[test]
fn the_workspace_loader_reports_the_operation_error() {
    let registry = KindRegistry::default();
    let err = registry
        .load(
            Path::new("m.rman.yaml"),
            &kafka("brokers: a:9092\ntopic: t\n"),
        )
        .unwrap_err();
    assert_eq!(err.message, OPERATION_ERROR);
    let loaded = registry
        .load(
            Path::new("m.rman.yaml"),
            &kafka("operation: consume\nbrokers: a:9092\ntopic: t\n"),
        )
        .unwrap();
    assert_eq!(loaded.kind(), "kafka");
    assert_eq!(loaded.name(), "orders");
}

#[test]
fn unknown_kinds_list_the_messaging_kinds() {
    let registry = KindRegistry::default();
    for kind in ["kafka", "mqtt", "amqp"] {
        assert!(registry.names().contains(&kind), "{kind}");
    }
    let err = registry
        .load(
            Path::new("m.rman.yaml"),
            "version: 1\nkind: nats\nname: x\n",
        )
        .unwrap_err();
    for kind in ["kafka", "mqtt", "amqp"] {
        assert!(err.message.contains(kind), "{}", err.message);
    }
}

#[test]
fn fields_of_other_kinds_or_operations_are_refused() {
    let cases = [
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\npartition: 1\n"),
            "kafka: 'partition' does not apply to consume requests",
        ),
        (
            kafka(
                "operation: produce\nbrokers: a:9092\ntopic: t\noffset: earliest\nmessages: [{value: 1}]\n",
            ),
            "kafka: 'offset' does not apply to produce requests",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\nqueue: q\n"),
            "kafka: 'queue' does not apply to consume requests",
        ),
        (
            kafka("operation: consume\ntopic: t\n"),
            "kafka: 'brokers' is required",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\n"),
            "kafka: 'topic' is required",
        ),
        (
            kafka("operation: produce\nbrokers: a:9092\ntopic: t\n"),
            "kafka: a produce needs at least one entry in 'messages'",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\nmessages: [{value: 1}]\n"),
            "kafka: 'messages' only applies to produce requests",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\nauth: {username: u}\n"),
            "kafka: credentials go in 'security', not 'auth'",
        ),
        (
            "version: 1\nkind: mqtt\nname: x\noperation: consume\ntopic: t\n".to_owned(),
            "mqtt: 'broker' is required",
        ),
        (
            "version: 1\nkind: amqp\nname: x\noperation: consume\nbroker: amqp://h\nbrokers: [a]\n"
                .to_owned(),
            "amqp: 'brokers' does not apply to consume requests",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\noffset: -1\n"),
            "offset must be earliest, latest",
        ),
        (
            kafka("operation: consume\nbrokers: []\ntopic: t\n"),
            "brokers must name at least one broker",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\nlimits: {duration_ms: 0}\n"),
            "limits.duration_ms must be at least 1",
        ),
        (
            kafka("operation: consume\nbrokers: a:9092\ntopic: t\nhost: x\n"),
            "unknown field",
        ),
    ];
    for (text, message) in cases {
        let err = parse(&text).unwrap_err();
        assert!(err.message.contains(message), "{text}: {}", err.message);
    }
}

#[test]
fn kafka_security_needs_sasl_fields_only_with_sasl_and_hides_the_password() {
    let r = parse(&kafka(
        "operation: consume\nbrokers: a:9093\ntopic: t\nsecurity: {protocol: SASL_SSL, mechanism: SCRAM-SHA-512, username: app, password: \"{{kafka_pw}}\"}\n",
    ))
    .unwrap();
    let security = r.security.as_deref().unwrap();
    assert_eq!(security.effective_protocol(), KafkaProtocol::SaslSsl);
    assert!(security.effective_protocol().uses_tls());
    assert_eq!(security.mechanism, Some(SaslMechanism::ScramSha512));
    assert!(!format!("{:?}", r.security).contains("kafka_pw"));
    let cases = [
        (
            "{protocol: SASL_PLAINTEXT, mechanism: PLAIN, username: u}",
            "security: SASL_PLAINTEXT requires 'password'",
        ),
        (
            "{protocol: SSL, username: u}",
            "security: 'username' needs protocol SASL_PLAINTEXT or SASL_SSL",
        ),
        ("{protocol: TLS}", "unknown variant"),
        (
            "{protocol: SASL_SSL, mechanism: GSSAPI, username: u, password: p}",
            "unknown variant",
        ),
    ];
    for (security, message) in cases {
        let err = parse(&kafka(&format!(
            "operation: consume\nbrokers: a:9093\ntopic: t\nsecurity: {security}\n"
        )))
        .unwrap_err();
        assert!(err.message.contains(message), "{security}: {}", err.message);
    }
}

#[test]
fn payloads_are_text_json_base64_or_files() {
    let r = parse(&kafka(
        "operation: produce\nbrokers: a:9092\ntopic: t\nmessages:\n  - value: plain text\n  - value: 42\n  - value: {base64: AAE=}\n  - {key: {file: ./key.bin}, value: {file: ./blob.bin}}\n  - value: {base64: AAE=, other: 1}\n  - key: k\n",
    ))
    .unwrap();
    let values: Vec<_> = r.message_list().into_iter().map(|m| m.value).collect();
    assert_eq!(values[0], Some(Payload::Text("plain text".into())));
    assert_eq!(values[1], Some(Payload::Json(json!(42))));
    assert_eq!(values[2], Some(Payload::Base64("AAE=".into())));
    let Some(Payload::File(path)) = &values[3] else {
        panic!("a file payload: {:?}", values[3]);
    };
    assert_eq!(path.get(), "./blob.bin");
    assert_eq!(
        values[4],
        Some(Payload::Json(json!({"base64": "AAE=", "other": 1})))
    );
    assert_eq!(values[5], None);
    assert_eq!(r.file_paths().len(), 2);
}

#[test]
fn schema_registry_settings_load() {
    let r = parse(&kafka(
        "operation: produce\nbrokers: a:9092\ntopic: t\nschema_registry: {url: \"http://{{sr}}\", subject: orders-value, schema_version: 3, auth: {type: basic, username: u, password: p}}\nmessages: [{value: {id: 1}}]\n",
    ))
    .unwrap();
    let registry = r.schema_registry.unwrap();
    assert_eq!(registry.subject.as_deref(), Some("orders-value"));
    assert_eq!(
        registry.schema_version.map(|v| v.path_segment()),
        Some("3".to_owned())
    );
    assert!(registry.auth.is_some());
}

fn mqtt(body: &str) -> String {
    format!("version: 1\nkind: mqtt\nname: m\nbroker: mqtt://localhost\n{body}")
}

fn amqp(body: &str) -> String {
    format!("version: 1\nkind: amqp\nname: a\nbroker: amqp://localhost\n{body}")
}

#[test]
fn mqtt_options_load_with_their_defaults() {
    let r = parse(&mqtt(
        "operation: produce\ntopic: sensors/room1/temp\nqos: 2\nretain: true\nproperties: {content_type: application/json, message_expiry: 60, response_topic: r/1}\nmessages: [{value: 21.5, headers: [{name: unit, value: C}]}]\n",
    ))
    .unwrap();
    assert_eq!(r.effective_qos(), Qos::EXACTLY_ONCE);
    assert_eq!(r.effective_protocol_version(), MqttVersion::V5);
    assert_eq!(r.retain, Some(true));
    assert_eq!(r.properties.unwrap().message_expiry, Some(60));
    let r = parse(&mqtt(
        "operation: consume\ntopic: \"sensors/#\"\nprotocol_version: \"3.1.1\"\nclient_id: fixed\nclean_session: false\n",
    ))
    .unwrap();
    assert_eq!(r.effective_protocol_version(), MqttVersion::V311);
    assert_eq!(r.effective_qos(), Qos::AT_LEAST_ONCE);
    assert_eq!(r.clean_session, Some(false));
}

#[test]
fn amqp_options_load() {
    let r = parse(&amqp(
        "operation: produce\nexchange: \"\"\nrouting_key: payments\nmandatory: true\nproperties: {content_type: text/plain, correlation_id: c-1, reply_to: replies, delivery_mode: 2, expiration: 60000}\nmessages: [{value: hi}]\n",
    ))
    .unwrap();
    assert_eq!(r.mandatory, Some(true));
    assert_eq!(r.properties.unwrap().delivery_mode, Some(2));
    let r = parse(&amqp(
        "operation: consume\nqueue: payments\nrequeue: true\n",
    ))
    .unwrap();
    assert!(!r.is_destructive_consume());
    let r = parse(&amqp(
        "operation: consume\nqueue: payments\ndestructive: true\n",
    ))
    .unwrap();
    assert!(r.is_destructive_consume());
    let r = parse(&amqp(
        "operation: consume\nexchange: orders\nbinding_key: \"order.*\"\n",
    ))
    .unwrap();
    assert!(!r.is_destructive_consume());
}

#[test]
fn a_named_queue_consume_without_opt_in_is_refused_at_load() {
    let err = parse(&amqp("operation: consume\nqueue: payments\n")).unwrap_err();
    assert_eq!(
        err.message,
        "amqp: consuming from an existing queue removes messages; set destructive: true or use requeue"
    );
    assert_eq!(err.message, NAMED_QUEUE_ERROR);
}

#[test]
fn mqtt_and_amqp_field_combinations_are_checked() {
    let cases = [
        (mqtt("operation: consume\n"), "mqtt: 'topic' is required"),
        (
            mqtt("operation: produce\ntopic: t\nmessages: [{key: k, value: 1}]\n"),
            "mqtt: messages have no 'key'",
        ),
        (
            mqtt("operation: consume\ntopic: t\nqos: 3\n"),
            "qos must be 0, 1 or 2",
        ),
        (
            mqtt("operation: consume\ntopic: t\nretain: true\n"),
            "mqtt: 'retain' does not apply to consume requests",
        ),
        (
            mqtt("operation: produce\ntopic: t\nclean_session: true\nmessages: [{value: 1}]\n"),
            "mqtt: 'clean_session' does not apply to produce requests",
        ),
        (
            mqtt("operation: consume\ntopic: t\nclean_session: false\n"),
            "mqtt: clean_session: false needs a fixed client_id",
        ),
        (
            mqtt(
                "operation: produce\ntopic: t\nprotocol_version: \"3.1.1\"\nmessages: [{value: 1, headers: [{name: a, value: b}]}]\n",
            ),
            "need protocol_version 5",
        ),
        (
            mqtt(
                "operation: produce\ntopic: t\nprotocol_version: \"3.1.1\"\nproperties: {content_type: a/b}\nmessages: [{value: 1}]\n",
            ),
            "mqtt: 'properties' need protocol_version 5",
        ),
        (
            mqtt(
                "operation: produce\ntopic: t\nproperties: {reply_to: r}\nmessages: [{value: 1}]\n",
            ),
            "mqtt: property 'reply_to' does not apply",
        ),
        (
            mqtt("operation: consume\ntopic: t\nprotocol_version: 4\n"),
            "protocol_version must be 3.1.1 or 5",
        ),
        (
            amqp("operation: produce\nexchange: x\nmessages: [{value: 1}]\n"),
            "amqp: 'routing_key' is required",
        ),
        (
            amqp(
                "operation: produce\nrouting_key: k\nproperties: {response_topic: r}\nmessages: [{value: 1}]\n",
            ),
            "amqp: property 'response_topic' does not apply",
        ),
        (
            amqp("operation: consume\n"),
            "amqp: a consume needs either 'exchange' or 'queue'",
        ),
        (
            amqp("operation: consume\nexchange: x\nqueue: q\ndestructive: true\n"),
            "either 'exchange' or 'queue'",
        ),
        (
            amqp("operation: consume\nexchange: x\nrequeue: true\n"),
            "apply to a named 'queue'",
        ),
        (
            amqp("operation: consume\nqueue: q\nack: auto\nrequeue: true\n"),
            "amqp: requeue needs ack: manual",
        ),
        (
            amqp(
                "operation: produce\nrouting_key: k\nmandatory: true\nqueue: q\nmessages: [{value: 1}]\n",
            ),
            "amqp: 'queue' does not apply to produce requests",
        ),
    ];
    for (text, message) in cases {
        let err = parse(&text).unwrap_err();
        assert!(err.message.contains(message), "{text}: {}", err.message);
    }
}

#[test]
fn an_asyncapi_reference_is_a_workspace_file() {
    let r = parse(&kafka(
        "operation: produce\nbrokers: a:9092\ntopic: orders\nasyncapi: {document: ../asyncapi.yaml, message: OrderCreated}\nmessages: [{value: {id: 1}}]\n",
    ))
    .unwrap();
    let asyncapi = r.asyncapi.as_ref().unwrap();
    assert_eq!(asyncapi.message, "OrderCreated");
    assert_eq!(r.file_paths().len(), 1);
    assert_eq!(r.file_paths()[0].get(), "../asyncapi.yaml");
}
