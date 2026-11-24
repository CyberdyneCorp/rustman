//! Calls against the in-process catalog (add-grpc-client tasks 3.1-3.6):
//! encoding, unary and streaming calls with their timeline, headless stream
//! bounds, deadlines and metadata.

mod common;

use std::time::{Duration, Instant};

use common::{Fixture, eventually, request_file};
use rustman_grpc::{CallType, Direction, GrpcError, StreamStop};
use rustman_test_support::grpc::{GrpcServer, Reflection};
use serde_json::json;

const GET: &str = "shop.v1.Catalog/GetProduct";
const LIST: &str = "shop.v1.Catalog/ListProducts";
const UPLOAD: &str = "shop.v1.Catalog/UploadProducts";
const CHAT: &str = "shop.v1.Catalog/Chat";

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_field_fails_before_any_network_io() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let err = fixture
        .prepare(&request_file(
            &server.target(),
            GET,
            "message: {idd: p-1}\n",
        ))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "grpc: unknown field 'idd' in shop.v1.GetProductRequest"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(server.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_unary_call_records_status_headers_trailers_and_timings() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let exchange = fixture
        .send(&request_file(&server.target(), GET, "message: {id: p-1}\n"))
        .await
        .unwrap();
    assert_eq!(exchange.protocol, "grpc");
    assert_eq!(exchange.call_type, CallType::Unary);
    assert_eq!(exchange.status.label(), "OK (0)");
    assert_eq!(
        exchange.response(),
        Some(&json!({
            "id": "p-1",
            "name": "Product p-1",
            "createdAt": "2026-11-17T10:00:00Z",
            "views": "42"
        }))
    );
    let header = |name: &str| exchange.headers.iter().find(|h| h.name == name);
    assert_eq!(header("x-served-by").unwrap().value, "catalog");
    assert!(
        exchange
            .trailers
            .iter()
            .any(|t| t.name == "grpc-status" && t.value == "0")
    );
    assert!(exchange.timeline.is_empty(), "unary calls keep no timeline");
    let timings = exchange.timings;
    assert!(timings.connect.is_some());
    assert!(timings.tls.is_none(), "grpc:// is h2c");
    assert!(timings.first_byte.unwrap() <= timings.total);
    let shown = serde_json::to_value(&exchange).unwrap();
    assert_eq!(shown["status"]["name"], "OK");
    assert_eq!(shown["call_type"], "unary");
    assert!(shown["timings"]["total_ms"].as_f64().unwrap() > 0.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_unary_call_shows_its_status() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let exchange = fixture
        .send(&request_file(
            &server.target(),
            GET,
            "message: {id: missing}\n",
        ))
        .await
        .unwrap();
    assert_eq!(
        exchange.status.render(),
        "NOT_FOUND (5): product missing not found"
    );
    assert!(exchange.messages.is_empty());
    assert!(!exchange.is_request_error());
}

#[tokio::test(flavor = "multi_thread")]
async fn server_streaming_collects_every_message_in_order() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let exchange = fixture
        .send(&request_file(
            &server.target(),
            LIST,
            "message: {pageSize: 3}\n",
        ))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
    let ids: Vec<&str> = exchange
        .messages
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["p-1", "p-2", "p-3"]);
    assert_eq!(exchange.document()["messages"].as_array().unwrap().len(), 3);
    let directions: Vec<Direction> = exchange.timeline.iter().map(|e| e.direction).collect();
    assert_eq!(
        directions,
        [
            Direction::Sent,
            Direction::Received,
            Direction::Received,
            Direction::Received
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_streaming_sends_every_message_then_half_closes() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let messages = "messages:\n  - {id: a}\n  - {id: b}\n  - {id: c}\n  - {id: d}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), UPLOAD, messages))
        .await
        .unwrap();
    assert_eq!(server.uploads(), [4], "4 messages, then half-close");
    assert_eq!(exchange.status.label(), "OK (0)");
    assert_eq!(exchange.response(), Some(&json!({"count": 4})));
    let sent = exchange
        .timeline
        .iter()
        .filter(|e| e.direction == Direction::Sent)
        .count();
    assert_eq!(sent, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bidi_timeline_has_one_entry_per_message() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let messages = "messages:\n  - {text: hi}\n  - {text: bye}\n";
    let exchange = fixture
        .send(&request_file(&server.target(), CHAT, messages))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
    assert_eq!(exchange.timeline.len(), 5);
    let count = |d: Direction| {
        exchange
            .timeline
            .iter()
            .filter(|e| e.direction == d)
            .count()
    };
    assert_eq!((count(Direction::Sent), count(Direction::Received)), (2, 3));
    assert!(exchange.timeline.iter().all(|e| e.size_bytes > 0));
    assert!(exchange.timeline.windows(2).all(|w| w[0].at <= w[1].at));
    let texts: Vec<&str> = exchange
        .messages
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["welcome", "echo: hi", "echo: bye"]);
    let entry = serde_json::to_value(&exchange.timeline[0]).unwrap();
    assert!(entry["at_ms"].is_number() && entry["size_bytes"].is_number());
    assert!(entry["direction"].is_string() && entry["message"].is_object());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endless_stream_stops_at_100_messages_and_is_cancelled() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let exchange = fixture
        .send(&request_file(
            &server.target(),
            LIST,
            "message: {pageSize: -1}\n",
        ))
        .await
        .unwrap();
    assert_eq!(exchange.messages.len(), 100);
    assert_eq!(exchange.stopped, Some(StreamStop::MaxMessages(100)));
    assert_eq!(exchange.status.label(), "CANCELLED (1)");
    assert!(!exchange.is_request_error());
    assert!(
        eventually(|| server.cancelled_streams() == 1).await,
        "the server sees the call cancelled"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_stops_after_its_duration() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    // A generous duration: even a slow runner receives a message in it.
    let rest = "message: {pageSize: -1}\nstream:\n  max_messages: 1000000\n  duration_ms: 1000\n";
    let started = Instant::now();
    let exchange = fixture
        .send(&request_file(&server.target(), LIST, rest))
        .await
        .unwrap();
    assert_eq!(exchange.stopped, Some(StreamStop::Duration(1000)));
    assert!(!exchange.messages.is_empty());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(eventually(|| server.cancelled_streams() == 1).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_deadline_is_deadline_exceeded() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "message: {id: slow}\ndeadline_ms: 200\n";
    let started = Instant::now();
    let exchange = fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    assert_eq!(exchange.status.label(), "DEADLINE_EXCEEDED (4)");
    assert!(
        exchange.is_request_error(),
        "counts as a request error (exit 3)"
    );
    // The server answers after 2 s: no response recorded means the client
    // gave up first, without a wall-clock margin a slow runner could miss.
    assert!(exchange.messages.is_empty(), "{:?}", exchange.messages);
    assert!(started.elapsed() < Duration::from_secs(10));
    let call = &server.calls()[0];
    let timeout = call.text("grpc-timeout").unwrap();
    assert!(
        timeout.ends_with('m') || timeout.ends_with('u'),
        "{timeout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_metadata_reaches_the_server_as_bytes() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "message: {id: p-1}\nmetadata:\n  - {name: trace-bin, value: AQID}\n  - {name: x-tenant, value: acme}\n";
    fixture
        .send(&request_file(&server.target(), GET, rest))
        .await
        .unwrap();
    let call = &server.calls()[0];
    assert_eq!(call.get("trace-bin"), Some([1u8, 2, 3].as_slice()));
    assert_eq!(call.text("x-tenant").as_deref(), Some("acme"));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_metadata_and_message_counts_fail_before_sending() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let rest = "message: {id: p-1}\nmetadata:\n  - {name: trace-bin, value: '%%%'}\n";
    let err = fixture
        .prepare(&request_file(&server.target(), GET, rest))
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("grpc: invalid metadata 'trace-bin'"),
        "{err}"
    );
    let rest = "messages:\n  - {id: a}\n  - {id: b}\n";
    let err = fixture
        .prepare(&request_file(&server.target(), GET, rest))
        .unwrap_err();
    assert!(matches!(err, GrpcError::Message(_)), "{err}");
    assert!(server.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_target_is_a_request_error() {
    let fixture = Fixture::new();
    let err = fixture
        .send(&request_file(
            "grpc://127.0.0.1:1",
            GET,
            "message: {id: p-1}\n",
        ))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("grpc: cannot connect to grpc://127.0.0.1:1"),
        "{err}"
    );
    assert_eq!(err.exit_code(), 3);
}

/// With trace propagation on, a native call carries `traceparent` and B3
/// as request metadata; metadata the call sets itself wins
/// (add-trace-propagation task 2.3).
#[tokio::test(flavor = "multi_thread")]
async fn trace_context_is_sent_as_request_metadata() {
    use rustman_core::trace::{PropagationFormat, TracePropagation};
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let explicit = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let plain = request_file(&server.target(), GET, "message: {id: p-1}\n");
    let own = format!(
        "message: {{id: p-1}}\nmetadata:\n  - {{name: traceparent, value: '{explicit}'}}\n"
    );
    let own = request_file(&server.target(), GET, &own);
    let mut recorded = Vec::new();
    for text in [&plain, &own] {
        let (call, mut options) = fixture.prepare(text).unwrap();
        options.http.trace = Some(TracePropagation {
            formats: vec![PropagationFormat::W3c, PropagationFormat::B3],
            ..TracePropagation::default()
        });
        let exchange = call.send(&options).await.unwrap();
        recorded.push(exchange.trace.expect("the call's trace context"));
    }
    let untraced = fixture.send(&plain).await.unwrap();
    assert_eq!(untraced.trace, None);
    let calls = server.calls();
    // Issue #119: the exchange keeps the context its metadata carried.
    for (call, sent) in calls.iter().zip(&recorded) {
        let ids = format!("00-{}-{}-01", sent.context.trace_id, sent.context.span_id);
        assert_eq!(call.text("traceparent"), Some(ids));
    }
    let traceparent = calls[0].text("traceparent").unwrap();
    assert!(traceparent.starts_with("00-") && traceparent.len() == 55);
    assert!(
        calls[0]
            .text("b3")
            .unwrap()
            .starts_with(&traceparent[3..35])
    );
    assert_eq!(calls[1].text("traceparent").as_deref(), Some(explicit));
    let b3 = calls[1].text("b3").unwrap();
    assert!(b3.starts_with("4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7"));
    assert_eq!(calls[2].text("traceparent"), None);
}
