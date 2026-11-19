//! Interactive calls (add-grpc-client task 6.2): the live timeline of a
//! running call, redacted, and a bidi stream kept open for messages typed
//! by the user until they half-close it.

mod common;

use common::{Fixture, request_file};
use rustman_core::Redactor;
use rustman_grpc::{Direction, Interactive};
use rustman_test_support::grpc::{GrpcServer, Reflection};
use serde_json::json;
use tokio::sync::mpsc;

const CHAT: &str = "shop.v1.Catalog/Chat";
const GET: &str = "shop.v1.Catalog/GetProduct";

#[tokio::test(flavor = "multi_thread")]
async fn a_bidi_call_sends_typed_messages_until_half_closed() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let text = request_file(&server.target(), CHAT, "messages:\n  - {text: hi}\n");
    let (call, mut options) = fixture.prepare(&text).unwrap();
    options.bounds = None;
    let (typed, outbound) = mpsc::unbounded_channel();
    let (feed, mut live) = mpsc::unbounded_channel();
    let interactive = Interactive::new(Some(outbound), Some(feed));
    let user = async move {
        // Wait for the echo of the file's message before typing.
        let mut received = 0;
        while let Some(entry) = live.recv().await {
            if entry.direction == Direction::Received {
                received += 1;
            }
            if received == 2 {
                break;
            }
        }
        typed.send(json!({"text": "more"})).unwrap();
        typed.send(json!({"nope": 1})).unwrap();
        // Dropping the sender half-closes the stream.
        drop(typed);
        live
    };
    let redactor = Redactor::default();
    let (exchange, mut live) = tokio::join!(interactive.send(&call, &options, &redactor), user);
    let exchange = exchange.unwrap();
    assert_eq!(exchange.status.label(), "OK (0)");
    let texts: Vec<&str> = exchange
        .messages
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["welcome", "echo: hi", "echo: more"]);
    let sent = exchange
        .timeline
        .iter()
        .filter(|e| e.direction == Direction::Sent)
        .count();
    assert_eq!(sent, 2, "the message the schema rejects is skipped");
    let mut rest = 0;
    while live.try_recv().is_ok() {
        rest += 1;
    }
    assert_eq!(rest, 2, "the live feed saw the typed message and its echo");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_live_feed_is_redacted() {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let text = request_file(&server.target(), GET, "message: {id: s3cr3t-value}\n");
    let (call, options) = fixture.prepare(&text).unwrap();
    let (feed, mut live) = mpsc::unbounded_channel();
    let interactive = Interactive::new(None, Some(feed));
    let mut redactor = Redactor::default();
    redactor.add_value("token", "s3cr3t-value");
    let exchange = interactive.send(&call, &options, &redactor).await.unwrap();
    assert_eq!(exchange.messages[0]["id"], "s3cr3t-value", "redacted later");
    let mut shown = Vec::new();
    while let Ok(entry) = live.try_recv() {
        shown.push(entry.message.to_string());
    }
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(
        shown.iter().all(|m| !m.contains("s3cr3t-value")),
        "{shown:?}"
    );
    assert!(shown[1].contains("«redacted:token»"), "{shown:?}");
}
