//! Captures, responses and mocks over the wire (spec: Capture any method
//! and path, Binary-safe bodies, Default response, Mock responses);
//! regressions for Ghostman S6-S8.

mod common;

use std::time::{Duration, Instant};

use bytes::Bytes;
use common::{SOON, body, header, raw, start, start_with, status};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use rustman_webhook::{Event, Limits, Mocks};
use tokio::net::TcpStream;

#[tokio::test]
async fn chunked_bodies_are_captured() {
    let l = start().await;
    let response = raw(
        l.addr(),
        b"POST /c HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n2\r\nbc\r\n0\r\n\r\n",
    )
    .await;
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(l.store.snapshot()[0].body.as_ref(), b"abc");
}

#[tokio::test]
async fn duplicate_headers_keep_arrival_order_and_case() {
    let l = start().await;
    raw(
        l.addr(),
        b"POST /d HTTP/1.1\r\nHost: x\r\nX-A: 1\r\nX-Other: o\r\nX-A: 2\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let capture = &l.store.snapshot()[0];
    let names: Vec<String> = capture
        .headers
        .iter()
        .map(|h| format!("{}: {}", h.name, h.value_text()))
        .collect();
    assert_eq!(
        names,
        [
            "Host: x",
            "X-A: 1",
            "X-Other: o",
            "X-A: 2",
            "Content-Length: 0"
        ]
    );
}

#[tokio::test]
async fn path_and_query_are_split_and_any_method_is_captured() {
    let l = start().await;
    raw(
        l.addr(),
        b"POST /hooks/stripe?debug=1 HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    raw(l.addr(), b"PURGE /x/y HTTP/1.1\r\nHost: x\r\n\r\n").await;
    let captures = l.store.snapshot();
    assert_eq!(captures[0].path, "/hooks/stripe");
    assert_eq!(captures[0].query.as_deref(), Some("debug=1"));
    assert_eq!(
        (captures[1].method.as_str(), captures[1].path.as_str()),
        ("PURGE", "/x/y")
    );
    assert_eq!(captures[1].query, None);
    assert_eq!((captures[0].id, captures[1].id), (1, 2));
    assert_eq!(captures[0].version_text(), "HTTP/1.1");
    assert!(captures[0].remote.ip().is_loopback());
    assert!(captures[0].timestamp().contains('T'));
}

#[tokio::test]
async fn non_utf8_bodies_round_trip_byte_for_byte() {
    let l = start().await;
    raw(
        l.addr(),
        b"POST /b HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n\xff\xfe\x00",
    )
    .await;
    let capture = &l.store.snapshot()[0];
    assert_eq!(capture.body.as_ref(), [0xFF, 0xFE, 0x00]);
    assert_eq!(capture.body_size(), 3);
    assert!(matches!(
        rustman_webhook::body::view(None, &capture.body),
        rustman_webhook::body::BodyView::Hex(_)
    ));
}

#[tokio::test]
async fn default_ack_on_the_wire() {
    let mut l = start().await;
    let response = raw(
        l.addr(),
        b"POST /anything HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}",
    )
    .await;
    assert_eq!(status(&response), 200);
    assert_eq!(body(&response), r#"{"status":"ok","source":"rustman"}"#);
    assert_eq!(header(&response, "content-length"), Some("34"));
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    let event = l.next_event(|e| matches!(e, Event::Captured(_))).await;
    assert!(
        event
            .to_string()
            .starts_with("#1 POST /anything from 127.0.0.1:"),
        "{event}"
    );
}

#[tokio::test]
async fn h2c_prior_knowledge_is_captured() {
    let l = start().await;
    let tcp = TcpStream::connect(l.addr()).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tcp),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let request = http::Request::post(format!("http://{}/h2?x=1", l.addr()))
        .header("x-a", "1")
        .header("x-a", "2")
        .body(Full::new(Bytes::from_static(b"hi")))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let ack = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(ack, rustman_webhook::DEFAULT_ACK);
    let capture = &l.store.snapshot()[0];
    assert_eq!(capture.version_text(), "HTTP/2.0");
    assert_eq!(
        (capture.path.as_str(), capture.query.as_deref()),
        ("/h2", Some("x=1"))
    );
    assert_eq!(capture.header_values("x-a").count(), 2);
    assert_eq!(capture.body.as_ref(), b"hi");
}

#[tokio::test]
async fn idle_connections_are_closed() {
    let limits = Limits {
        idle_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let l = start_with(limits, Mocks::empty()).await;
    let tcp = TcpStream::connect(l.addr()).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tcp),
    )
    .await
    .unwrap();
    let driver = tokio::spawn(conn);
    let request = http::Request::get(format!("http://{}/", l.addr()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(sender.send_request(request).await.unwrap().status(), 200);
    // The server closes the idle connection, which ends the client driver.
    tokio::time::timeout(SOON, driver)
        .await
        .expect("idle connection still open")
        .unwrap()
        .ok();
}

fn mocks(dir: &std::path::Path, yaml: &str) -> Mocks {
    let file = dir.join("mocks.yaml");
    std::fs::write(&file, yaml).unwrap();
    Mocks::from_file(&file).unwrap()
}

#[tokio::test]
async fn a_mocked_failure_answers_after_its_delay_and_is_still_captured() {
    let dir = tempfile::tempdir().unwrap();
    let set = mocks(
        dir.path(),
        "mocks:\n- {method: POST, path: /hooks/github, status: 500, delay_ms: 2000, body: boom}\n",
    );
    let mut l = start_with(Limits::default(), set).await;
    let started = Instant::now();
    let response = raw(
        l.addr(),
        b"POST /hooks/github HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}",
    )
    .await;
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(status(&response), 500);
    assert_eq!(body(&response), "boom");
    assert_eq!(header(&response, "content-length"), Some("4"));
    let capture = &l.store.snapshot()[0];
    assert_eq!(
        (capture.status, capture.mock.as_deref()),
        (500, Some("/hooks/github"))
    );
    let event = l
        .next_event(|e| matches!(e, Event::MockMatched { .. }))
        .await;
    assert_eq!(
        event.to_string(),
        "#1 matched mock POST /hooks/github -> 500 after 2000 ms"
    );
}

#[tokio::test]
async fn editing_mocks_yaml_reloads_without_restarting() {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("webhook");
    std::fs::create_dir(&dir).unwrap();
    let set = mocks(&dir, "mocks:\n- {path: /hook, status: 500}\n");
    let mut l = start_with(Limits::default(), set.clone()).await;
    let _watcher = rustman_webhook::watch::watch(ws.path(), set.clone(), l.store.clone()).unwrap();
    let request = b"POST /hook HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(status(&raw(l.addr(), request).await), 500);

    std::fs::write(
        dir.join("mocks.yaml"),
        "mocks:\n- {path: /hook, status: 503}\n",
    )
    .unwrap();
    let reloaded = l
        .next_event(|e| {
            matches!(e, Event::MocksReloaded { .. })
                && set.find("POST", "/hook").is_some_and(|m| m.status == 503)
        })
        .await;
    assert_eq!(reloaded, Event::MocksReloaded { count: 1 });
    assert_eq!(status(&raw(l.addr(), request).await), 503);

    std::fs::write(dir.join("mocks.yaml"), "mocks: [").unwrap();
    l.next_event(|e| matches!(e, Event::MocksInvalid { .. }))
        .await;
    assert_eq!(
        status(&raw(l.addr(), request).await),
        503,
        "previous mocks stay active"
    );
}

#[tokio::test]
async fn a_webhook_folder_created_after_start_is_watched() {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("webhook");
    // Changes elsewhere in the workspace are not watched for.
    std::fs::create_dir(ws.path().join("requests")).unwrap();
    let set = Mocks::from_file(&dir.join("mocks.yaml")).unwrap();
    let mut l = start_with(Limits::default(), set.clone()).await;
    let _watcher = rustman_webhook::watch::watch(ws.path(), set.clone(), l.store.clone()).unwrap();
    std::fs::write(ws.path().join("requests/a.rman.yaml"), "x").unwrap();

    std::fs::create_dir(&dir).unwrap();
    // The folder appearing reloads (no file yet) and starts its watch.
    l.next_event(|e| matches!(e, Event::MocksReloaded { count: 0 }))
        .await;
    std::fs::write(
        dir.join("mocks.yaml"),
        "mocks:\n- {path: /hook, status: 503}\n",
    )
    .unwrap();
    l.next_event(|_| set.find("POST", "/hook").is_some_and(|m| m.status == 503))
        .await;
    let request = b"POST /hook HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(status(&raw(l.addr(), request).await), 503);
}
