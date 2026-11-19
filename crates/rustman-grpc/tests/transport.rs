//! Transports (add-grpc-client tasks 4.1, 4.2): h2c, TLS verified by the
//! shared `TlsSettings` builder (custom CA, mTLS, insecure), and gRPC-Web
//! over rustman-http, against in-process tonic servers.

mod common;

use common::{Fixture, request_file};
use rustman_grpc::{CallType, GrpcError};
use rustman_test_support::TestCerts;
use rustman_test_support::grpc::{GrpcServer, Reflection, ServerTls};
use serde_json::json;

const GET: &str = "shop.v1.Catalog/GetProduct";
const LIST: &str = "shop.v1.Catalog/ListProducts";
const CHAT: &str = "shop.v1.Catalog/Chat";

fn with_certs(certs: &TestCerts) -> Fixture {
    let fixture = Fixture::new();
    fixture.write("certs/ca.pem", &certs.ca_pem);
    fixture.write("certs/client.pem", &certs.client_cert_pem);
    fixture.write("certs/client.key", &certs.client_key_pem);
    fixture
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grpc_target_uses_h2c() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let exchange = fixture
        .send(&request_file(&server.target(), GET, "message: {id: p-1}\n"))
        .await
        .unwrap();
    assert!(server.target().starts_with("grpc://"));
    assert_eq!(exchange.transport, "grpc");
    assert_eq!(exchange.status.label(), "OK (0)");
    assert!(exchange.timings.tls.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_signed_certificate_is_rejected_naming_the_target() {
    let certs = TestCerts::generate();
    let server = GrpcServer::start_tls(Reflection::None, ServerTls::SelfSigned, &certs).await;
    let fixture = with_certs(&certs);
    let err = fixture
        .send(&request_file(&server.target(), GET, "message: {id: p-1}\n"))
        .await
        .unwrap_err();
    assert!(matches!(err, GrpcError::Tls { .. }), "{err}");
    let shown = err.to_string();
    assert!(
        shown.starts_with(&format!("grpc: TLS error at {}: ", server.target())),
        "{shown}"
    );
    assert!(shown.contains("certificate"), "{shown}");
    assert_eq!(err.exit_code(), 3);
    assert!(server.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn insecure_accepts_a_self_signed_certificate() {
    let certs = TestCerts::generate();
    let server = GrpcServer::start_tls(Reflection::None, ServerTls::SelfSigned, &certs).await;
    let fixture = with_certs(&certs);
    let rest = "message: {id: p-1}\ntls:\n  insecure: true\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custom_ca_verifies_the_server() {
    let certs = TestCerts::generate();
    let server = GrpcServer::start_tls(Reflection::None, ServerTls::Ca, &certs).await;
    let fixture = with_certs(&certs);
    let rest = "message: {id: p-1}\ntls:\n  ca_bundle: certs/ca.pem\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
    assert!(exchange.timings.tls.is_some(), "the handshake is timed");
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_enabled_on_a_bare_target() {
    let certs = TestCerts::generate();
    let server = GrpcServer::start_tls(Reflection::None, ServerTls::Ca, &certs).await;
    let fixture = with_certs(&certs);
    let target = format!("localhost:{}", server.addr().port());
    let rest = "message: {id: p-1}\ntls:\n  enabled: true\n  ca_bundle: certs/ca.pem\n";
    let exchange = fixture
        .send(&request_file(&target, GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
}

#[tokio::test(flavor = "multi_thread")]
async fn mutual_tls_presents_the_client_certificate() {
    let certs = TestCerts::generate();
    let server = GrpcServer::start_tls(Reflection::None, ServerTls::Mutual, &certs).await;
    let fixture = with_certs(&certs);
    let rest = "message: {id: p-1}\ntls:\n  ca_bundle: certs/ca.pem\n  client_cert: certs/client.pem\n  client_key: certs/client.key\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");

    let rest = "message: {id: p-1}\ntls:\n  ca_bundle: certs/ca.pem\n";
    let refused = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await;
    match refused {
        Err(err) => assert_eq!(err.exit_code(), 3, "{err}"),
        Ok(exchange) => assert_ne!(exchange.status.code, 0, "{exchange:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_unary_call() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest =
        "transport: grpc-web\nmessage: {id: p-1}\nmetadata:\n  - {name: trace-bin, value: AQID}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.transport, "grpc-web");
    assert_eq!(exchange.status.label(), "OK (0)");
    assert_eq!(exchange.response().unwrap()["id"], json!("p-1"));
    assert!(
        exchange
            .trailers
            .iter()
            .any(|t| t.name == "grpc-status" && t.value == "0")
    );
    let call = &server.calls()[0];
    assert_eq!(call.get("trace-bin"), Some([1u8, 2, 3].as_slice()));
}

/// A gRPC-Web response frame longer than `[http] max_body_bytes` is
/// refused instead of buffered.
#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_frames_are_bounded_by_max_body_bytes() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    fixture.write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nmax_body_bytes = 4\n",
    );
    let rest = "transport: grpc-web\nmessage: {id: p-1}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "RESOURCE_EXHAUSTED (8)");
    assert!(
        exchange
            .status
            .message
            .contains("exceeds the limit of 4 bytes"),
        "{}",
        exchange.status.message
    );
    assert!(exchange.messages.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_expired_deadline_is_deadline_exceeded() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "transport: grpc-web\nmessage: {id: slow}\ndeadline_ms: 200\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "DEADLINE_EXCEEDED (4)");
    assert!(exchange.is_request_error(), "counts as a request error");
    // The server answers after 2 s: nothing received means the client gave
    // up first.
    assert!(exchange.messages.is_empty(), "{:?}", exchange.messages);
    assert_eq!(
        server.calls()[0].text("grpc-timeout").as_deref(),
        Some("200m")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_server_streaming_call() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "transport: grpc-web\nmessage: {pageSize: 3}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), LIST, rest))
        .await
        .unwrap();
    assert_eq!(exchange.call_type, CallType::ServerStreaming);
    assert_eq!(exchange.status.label(), "OK (0)");
    assert_eq!(exchange.messages.len(), 3);
    assert_eq!(exchange.timeline.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_error_status_with_details() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "transport: grpc-web\nmessage: {id: invalid}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "INVALID_ARGUMENT (3)");
    assert_eq!(exchange.status.message, "invalid product id");
    assert_eq!(
        exchange.status.details[0].lines(),
        ["field violation: id: must start with p-"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_stream_bounds_apply() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "transport: grpc-web\nmessage: {pageSize: -1}\nstream:\n  max_messages: 5\n";
    let exchange = fixture
        .send(&request_file(&server.target(), LIST, rest))
        .await
        .unwrap();
    assert_eq!(exchange.messages.len(), 5);
    assert_eq!(exchange.status.label(), "CANCELLED (1)");
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_web_rejects_bidi_before_sending() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "transport: grpc-web\nmessages:\n  - {text: hi}\n";
    let err = fixture
        .prepare(&request_file(&server.target(), CHAT, rest))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "grpc: grpc-web supports only unary and server-streaming calls"
    );
    assert!(server.calls().is_empty());
}
