//! SSE sessions against the in-process test server (add-realtime-client
//! tasks 3.2, 3.3 and 4.2): GET and POST streams, the content-type check,
//! resume with `Last-Event-ID` after a drop, `204 No Content`, the
//! reconnect budget, headless bounds and the event size limit.

use std::time::Duration;

use rustman_core::model::{RealtimeLimits, Reconnect, RequestSettings};
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, ResolvedSse, TlsFiles};
use rustman_http::{HttpEngine, SendOptions};
use rustman_realtime::{Bounds, Direction, RealtimeExchange, SessionOptions, run_sse};
use rustman_test_support::{RealtimeSseServer, SseReply};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn target(url: &str) -> ResolvedSse {
    ResolvedSse {
        http: ResolvedRequest {
            method: "GET".to_owned(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        },
        last_event_id: None,
        reconnect: Reconnect::default(),
        limits: RealtimeLimits::default(),
    }
}

/// `target` with at most `attempts` reconnections.
fn reconnecting(url: &str, attempts: u32) -> ResolvedSse {
    let mut sse = target(url);
    sse.reconnect.max_attempts = Some(attempts);
    sse
}

fn headless(messages: u64) -> SessionOptions {
    SessionOptions::headless(
        Bounds {
            max_messages: messages,
            duration: Duration::from_secs(20),
        },
        CancellationToken::new(),
    )
}

async fn run(target: &ResolvedSse, messages: u64) -> RealtimeExchange {
    let engine = HttpEngine::new();
    run_sse(&engine, target, &SendOptions::default(), headless(messages)).await
}

fn received(exchange: &RealtimeExchange) -> Vec<(String, Option<String>, String)> {
    exchange
        .timeline
        .entries()
        .filter(|e| e.dir == Direction::Received)
        .map(|e| (e.kind.clone(), e.id.clone(), e.payload.clone()))
        .collect()
}

fn notes(exchange: &RealtimeExchange) -> Vec<String> {
    exchange
        .timeline
        .entries()
        .filter(|e| e.dir == Direction::System)
        .map(|e| format!("{}: {}", e.kind, e.payload))
        .collect()
}

#[tokio::test]
async fn events_are_recorded_with_type_id_and_data() {
    let stream = ": hi\n\nevent: update\ndata: line1\ndata: line2\nid: 7\n\ndata: plain\n\n";
    let server = RealtimeSseServer::start(vec![SseReply::events(stream)]).await;
    let exchange = run(&reconnecting(&server.url("/events"), 0), 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(
        received(&exchange),
        [
            (
                "update".to_owned(),
                Some("7".to_owned()),
                "line1\nline2".to_owned()
            ),
            (
                "message".to_owned(),
                Some("7".to_owned()),
                "plain".to_owned()
            ),
        ]
    );
    assert_eq!(exchange.received, 2);
    assert!(exchange.first_message_ms.is_some());
    let handshake = exchange.handshake.as_ref().unwrap();
    assert_eq!(handshake.status, 200);
    assert_eq!(handshake.header("content-type"), Some("text/event-stream"));
    assert_eq!(exchange.end.unwrap().reason, "stream ended");
    assert_eq!(server.hits()[0].header("accept-encoding"), Some("identity"));
}

#[tokio::test]
async fn a_post_stream_sends_its_body() {
    let server =
        RealtimeSseServer::start(vec![SseReply::events("data: {\"token\":\"a\"}\n\n")]).await;
    let mut sse = reconnecting(&server.url("/v1/chat"), 0);
    sse.http.method = "POST".to_owned();
    sse.http.body = ResolvedBody::Json(json!({"stream": true}));
    let exchange = run(&sse, 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(received(&exchange)[0].2, r#"{"token":"a"}"#);
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.json(), json!({"stream": true}));
}

#[tokio::test]
async fn a_response_that_is_not_an_event_stream_fails() {
    let reply = SseReply::events("{}").content_type("application/json");
    let server = RealtimeSseServer::start(vec![reply]).await;
    let exchange = run(&target(&server.url("/")), 50).await;
    let error = exchange.error.unwrap().to_string();
    assert_eq!(error, "sse: unexpected content type application/json");
    assert_eq!(server.hits().len(), 1, "a failed stream is not retried");
}

#[tokio::test]
async fn an_error_status_fails() {
    let server = RealtimeSseServer::start(vec![SseReply::status(401)]).await;
    let exchange = run(&target(&server.url("/")), 50).await;
    let error = exchange.error.unwrap().to_string();
    assert_eq!(error, "sse: unexpected status 401 Unauthorized");
    assert_eq!(exchange.handshake.unwrap().status, 401);
}

#[tokio::test]
async fn a_dropped_stream_resumes_with_the_last_event_id() {
    let first = SseReply::events("retry: 10\nid: 41\ndata: a\n\nid: 42\ndata: b\n\n");
    let second = SseReply::events("data: c\n\n");
    let server = RealtimeSseServer::start(vec![first, second]).await;
    let exchange = run(&reconnecting(&server.url("/"), 1), 50).await;
    assert_eq!(exchange.error, None);
    let hits = server.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].header("last-event-id"), None);
    assert_eq!(hits[1].header("last-event-id"), Some("42"));
    let data: Vec<String> = received(&exchange).into_iter().map(|e| e.2).collect();
    assert_eq!(data, ["a", "b", "c"]);
    let notes = notes(&exchange);
    assert!(
        notes.contains(&"reconnect: reconnected (attempt 1, Last-Event-ID: 42)".to_owned()),
        "{notes:?}"
    );
    assert_eq!(exchange.end.unwrap().reason, "stream ended");
}

/// The `id` of an event the drop cut before its blank line was never
/// dispatched, so it does not move `Last-Event-ID` (WHATWG).
#[tokio::test]
async fn an_unfinished_event_does_not_move_the_last_event_id() {
    let first = SseReply::events("retry: 10\nid: 41\ndata: a\n\nid: 42\ndata: cut\n");
    let second = SseReply::events("data: b\n\n");
    let server = RealtimeSseServer::start(vec![first, second]).await;
    let exchange = run(&reconnecting(&server.url("/"), 1), 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(server.hits()[1].header("last-event-id"), Some("41"));
    let data: Vec<String> = received(&exchange).into_iter().map(|e| e.2).collect();
    assert_eq!(data, ["a", "b"]);
}

#[tokio::test]
async fn the_file_last_event_id_is_sent_first() {
    let server = RealtimeSseServer::start(vec![SseReply::events("data: a\n\n")]).await;
    let mut sse = reconnecting(&server.url("/"), 0);
    sse.last_event_id = Some("41".to_owned());
    let exchange = run(&sse, 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(server.hits()[0].header("last-event-id"), Some("41"));
}

#[tokio::test]
async fn a_204_stops_reconnecting_and_ends_normally() {
    let first = SseReply::events("retry: 10\nid: 1\ndata: a\n\n");
    let server = RealtimeSseServer::start(vec![first, SseReply::status(204)]).await;
    let exchange = run(&reconnecting(&server.url("/"), 5), 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(server.hits().len(), 2);
    assert_eq!(
        exchange.end.unwrap().reason,
        "server asked to stop: 204 No Content"
    );
}

#[tokio::test]
async fn reconnects_stop_after_max_attempts() {
    let server = RealtimeSseServer::start(vec![SseReply::events("retry: 5\ndata: a\n\n")]).await;
    let exchange = run(&reconnecting(&server.url("/"), 2), 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(
        server.hits().len(),
        3,
        "the first connection and 2 attempts"
    );
    assert_eq!(exchange.received, 3);
}

#[tokio::test]
async fn zero_attempts_disable_reconnecting() {
    let server = RealtimeSseServer::start(vec![SseReply::events("retry: 5\ndata: a\n\n")]).await;
    let exchange = run(&reconnecting(&server.url("/"), 0), 50).await;
    assert_eq!(exchange.error, None);
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test]
async fn an_endless_feed_stops_at_max_messages() {
    let reply = SseReply::forever(r#"{"n":{n}}"#, Duration::from_millis(1));
    let server = RealtimeSseServer::start(vec![reply]).await;
    let sse = target(&server.url("/"));
    let options = SessionOptions::headless(Bounds::of(&sse.limits), CancellationToken::new());
    let exchange = run_sse(&HttpEngine::new(), &sse, &SendOptions::default(), options).await;
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.received, 50);
    assert_eq!(exchange.end.unwrap().reason, "max_messages reached (50)");
}

#[tokio::test]
async fn an_oversized_event_aborts_the_stream() {
    let big = format!("data: {}\n\n", "x".repeat(2 * 1024 * 1024));
    let server = RealtimeSseServer::start(vec![SseReply::events(&big)]).await;
    let mut sse = target(&server.url("/"));
    sse.limits.max_message_bytes = Some(1_048_576);
    let exchange = run(&sse, 50).await;
    let error = exchange.error.clone().unwrap().to_string();
    assert!(error.starts_with("sse: message too large: "), "{error}");
    let notes = notes(&exchange);
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("close: message too large: ")),
        "{notes:?}"
    );
    assert_eq!(server.hits().len(), 1, "an aborted stream is not resumed");
}

#[tokio::test]
async fn a_cancelled_session_ends_without_error() {
    let server = RealtimeSseServer::start(vec![SseReply::events("data: a\n\n").open()]).await;
    let cancel = CancellationToken::new();
    let options = SessionOptions::headless(
        Bounds {
            max_messages: 50,
            duration: Duration::from_secs(20),
        },
        cancel.clone(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let options = SessionOptions {
        feed: Some(tx),
        ..options
    };
    let sse = target(&server.url("/"));
    let engine = HttpEngine::new();
    let send = SendOptions::default();
    let session = run_sse(&engine, &sse, &send, options);
    let watcher = async {
        while let Some(entry) = rx.recv().await {
            if entry.dir == Direction::Received {
                cancel.cancel();
            }
        }
    };
    let (exchange, ()) = tokio::join!(session, watcher);
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.end.unwrap().reason, "cancelled");
}

#[tokio::test]
async fn a_huge_retry_is_capped_with_a_note() {
    let reply = SseReply::events("retry: 99999999999999999999999\ndata: a\n\n");
    let server = RealtimeSseServer::start(vec![reply]).await;
    let sse = reconnecting(&server.url("/"), 1);
    let bounds = Bounds {
        max_messages: 50,
        duration: Duration::from_secs(2),
    };
    let options = SessionOptions::headless(bounds, CancellationToken::new());
    let exchange = run_sse(&HttpEngine::new(), &sse, &SendOptions::default(), options).await;
    assert_eq!(exchange.error, None);
    let notes = notes(&exchange);
    let capped = format!("error: retry of {} ms capped at 60000 ms", u64::MAX);
    assert_eq!(
        notes.iter().filter(|n| **n == capped).count(),
        1,
        "{notes:?}"
    );
    assert_eq!(exchange.end.unwrap().reason, "duration reached (2000 ms)");
}
