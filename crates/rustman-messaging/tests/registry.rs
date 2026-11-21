//! The Schema Registry client against a fake registry on loopback
//! (add-messaging-client task 2.4): Avro id 7 decoded to JSON, Protobuf
//! and JSON Schema payloads, encoding with a subject, the per-run cache,
//! and the raw-bytes warning when a schema cannot be fetched.

mod common;

use apache_avro::types::Value as AvroValue;
use rustman_messaging::registry::{RegistryClient, split_wire, wire_header};
use rustman_test_support::graphql_http::Reply;
use rustman_test_support::{GraphqlHttpServer, RecordedRequest};
use serde_json::{Value, json};

const ORDER_AVRO: &str = r#"{"type":"record","name":"Order","fields":[{"name":"id","type":"int"},{"name":"type","type":"string"}]}"#;
const ORDER_PROTO: &str =
    "syntax = \"proto3\";\nmessage Order { int32 id = 1; string type = 2; }\n";

fn answer(request: &RecordedRequest) -> Reply {
    let json = |body: Value| {
        Reply::raw(
            200,
            "application/vnd.schemaregistry.v1+json",
            &body.to_string(),
        )
    };
    match request.uri.as_str() {
        "/schemas/ids/7" => json(json!({"schema": ORDER_AVRO})),
        "/schemas/ids/8" => json(json!({"schema": ORDER_PROTO, "schemaType": "PROTOBUF"})),
        "/schemas/ids/9" => json(json!({"schema": "{\"type\":\"object\"}", "schemaType": "JSON"})),
        "/subjects/orders-value/versions/latest" => {
            json(json!({"subject": "orders-value", "version": 3, "id": 7, "schema": ORDER_AVRO}))
        }
        _ => Reply::raw(
            404,
            "application/json",
            r#"{"error_code":40403,"message":"Schema not found"}"#,
        ),
    }
}

fn client(server: &GraphqlHttpServer, extra: &str) -> RegistryClient {
    let text = format!(
        "version: 1\nkind: kafka\nname: r\noperation: consume\nbrokers: k:9092\ntopic: t\nschema_registry:\n  url: '{}'\n{extra}",
        server.url("/")
    );
    let resolved = common::resolve(&text, "");
    RegistryClient::new(resolved.schema_registry.as_ref().unwrap()).unwrap()
}

fn avro_order(id: i32, kind: &str) -> Vec<u8> {
    let schema = apache_avro::Schema::parse_str(ORDER_AVRO).unwrap();
    let value = AvroValue::Record(vec![
        ("id".into(), AvroValue::Int(id)),
        ("type".into(), AvroValue::String(kind.into())),
    ]);
    let writer = apache_avro::writer::datum::GenericDatumWriter::builder(&schema)
        .build()
        .unwrap();
    let mut bytes = wire_header(7);
    bytes.extend(writer.write_value_to_vec(value).unwrap());
    bytes
}

#[tokio::test]
async fn avro_id_7_decodes_to_json_with_the_schema_id_and_is_fetched_once() {
    let server = GraphqlHttpServer::start(answer).await;
    let registry = client(&server, "");
    let bytes = avro_order(1, "OrderCreated");
    let decoded = registry.decode(&bytes).await.unwrap().unwrap();
    assert_eq!(decoded.schema_id, 7);
    assert_eq!(decoded.value, json!({"id": 1, "type": "OrderCreated"}));
    let again = registry
        .decode(&avro_order(2, "OrderPaid"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.value["id"], 2);
    let hits = server.hits();
    assert_eq!(hits.len(), 1, "cached per run: {hits:?}");
    assert_eq!(hits[0].uri, "/schemas/ids/7");
    assert_eq!(
        hits[0].header("accept"),
        Some("application/vnd.schemaregistry.v1+json")
    );
}

#[tokio::test]
async fn protobuf_and_json_schema_payloads_decode() {
    let server = GraphqlHttpServer::start(answer).await;
    let registry = client(&server, "");
    // Order { id: 5, type: "x" } after the [0] index path.
    let mut proto = wire_header(8);
    proto.extend([0, 0x08, 0x05, 0x12, 0x01, b'x']);
    let decoded = registry.decode(&proto).await.unwrap().unwrap();
    assert_eq!(
        (decoded.schema_id, decoded.value),
        (8, json!({"id": 5, "type": "x"}))
    );
    let mut json_payload = wire_header(9);
    json_payload.extend(br#"{"id":3}"#);
    let decoded = registry.decode(&json_payload).await.unwrap().unwrap();
    assert_eq!(decoded.value, json!({"id": 3}));
    assert!(registry.decode(br#"{"plain":true}"#).await.is_none());
}

#[tokio::test]
async fn a_failed_fetch_is_a_warning_with_the_raw_bytes_kept() {
    let server = GraphqlHttpServer::start(answer).await;
    let registry = client(&server, "");
    let mut bytes = wire_header(42);
    bytes.extend([1, 2, 3]);
    let warning = registry.decode(&bytes).await.unwrap().unwrap_err();
    assert_eq!(
        warning,
        "schema registry: schema id 42: the registry answered HTTP 404; showing the raw bytes"
    );
    assert_eq!(split_wire(&bytes), Some((42, &[1u8, 2, 3][..])));
}

#[tokio::test]
async fn a_subject_encodes_values_with_its_latest_schema() {
    let server = GraphqlHttpServer::start(answer).await;
    let registry = client(&server, "  subject: orders-value\n");
    let (id, bytes) = registry
        .encode(&json!({"id": 4, "type": "OrderCreated"}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, 7);
    assert_eq!(bytes, avro_order(4, "OrderCreated"));
    let decoded = registry.decode(&bytes).await.unwrap().unwrap();
    assert_eq!(decoded.value["type"], "OrderCreated");
    let uris: Vec<String> = server.hits().into_iter().map(|h| h.uri).collect();
    assert_eq!(uris, ["/subjects/orders-value/versions/latest"]);
    let err = registry
        .encode(&json!({"id": "nope"}))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("schema registry: subject 'orders-value': value does not match"),
        "{err}"
    );
    let plain = client(&server, "");
    assert!(plain.encode(&json!({})).await.is_none());
}

#[tokio::test]
async fn registry_credentials_are_sent_and_never_shown() {
    let server = GraphqlHttpServer::start(answer).await;
    let registry = client(
        &server,
        "  auth: {type: basic, username: sr, password: '{{kafka_pw}}'}\n",
    );
    let mut bytes = wire_header(42);
    bytes.push(0);
    let warning = registry.decode(&bytes).await.unwrap().unwrap_err();
    assert!(!warning.contains(common::KAFKA_PW), "{warning}");
    let hits = server.hits();
    let authorization = hits[0].header("authorization").unwrap();
    assert!(authorization.starts_with("Basic "), "{authorization}");
}
