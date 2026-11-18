//! Replaying captures through `rustman-http` to a local server (spec:
//! Replay with header hygiene), including the Ghostman B1 regression.

mod common;

use bytes::Bytes;
use common::raw;
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::TestServer;
use rustman_webhook::{Capture, replay};
use serde_json::Value;

/// Capture one request sent to a webhook listener as raw bytes.
async fn capture(request: &[u8]) -> Capture {
    let l = common::start().await;
    raw(l.addr(), request).await;
    l.store.snapshot()[0].as_ref().clone()
}

fn echoed_header<'a>(echo: &'a Value, name: &str) -> Vec<&'a str> {
    echo["headers"]
        .as_array()
        .expect("headers")
        .iter()
        .filter(|h| h[0].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
        .filter_map(|h| h[1].as_str())
        .collect()
}

#[tokio::test]
async fn replay_after_editing_the_body_sends_the_target_host_and_a_correct_length() {
    let mut captured = capture(
        b"POST /hooks/x HTTP/1.1\r\nHost: 127.0.0.1:9090\r\nContent-Type: application/json\r\nConnection: keep-alive, X-Hop\r\nX-Hop: 1\r\nX-Event: push\r\nContent-Length: 7\r\n\r\n{\"a\":1}",
    )
    .await;
    captured.body = Bytes::from_static(br#"{"a":12345}"#);

    let server = TestServer::http().await;
    let url = server.url("/echo");
    let exchange = replay(&HttpEngine::new(), &captured, &url, &SendOptions::default())
        .await
        .expect("the edited capture replays without framing errors");
    assert_eq!(exchange.status, 200);
    assert!(exchange.warnings.is_empty(), "{:?}", exchange.warnings);

    let echo: Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["body"], r#"{"a":12345}"#);
    let host = format!("localhost:{}", server.port());
    assert_eq!(echoed_header(&echo, "host"), [host.as_str()]);
    assert_eq!(echoed_header(&echo, "content-length"), ["11"]);
    assert_eq!(echoed_header(&echo, "x-event"), ["push"]);
    assert!(echoed_header(&echo, "x-hop").is_empty());
    assert_eq!(echoed_header(&echo, "connection"), Vec::<&str>::new());
}

#[tokio::test]
async fn binary_bodies_replay_byte_for_byte() {
    let captured = capture(
        b"PUT /bin HTTP/1.1\r\nHost: x\r\nContent-Type: application/octet-stream\r\nContent-Length: 3\r\n\r\n\xff\xfe\x00",
    )
    .await;
    let server = TestServer::http().await;
    let exchange = replay(
        &HttpEngine::new(),
        &captured,
        &server.url("/echo"),
        &SendOptions::default(),
    )
    .await
    .unwrap();
    let echo: Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echo["method"], "PUT");
    assert_eq!(echoed_header(&echo, "content-length"), ["3"]);
    assert_eq!(
        echoed_header(&echo, "content-type"),
        ["application/octet-stream"]
    );
}
