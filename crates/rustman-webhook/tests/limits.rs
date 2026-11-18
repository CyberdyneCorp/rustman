//! Limits and malformed input over raw sockets (spec: Standards-compliant
//! HTTP handling, Body size limit, Header limits, Timeouts, Event log);
//! regressions for Ghostman S1-S4.

mod common;

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use common::{body, raw, read_to_close, start, start_with, status};
use http_body_util::Full;
use hyper_util::rt::TokioIo;
use rustman_webhook::mocks::parse;
use rustman_webhook::{Event, Limits, Mocks};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

fn post_with_headers(count: usize) -> Vec<u8> {
    let mut head = String::from("POST /h HTTP/1.1\r\nHost: x\r\n");
    // Host and Content-Length are two of the `count` fields.
    for i in 2..count {
        let _ = write!(head, "X-H{i}: v\r\n");
    }
    head.push_str("Content-Length: 0\r\n\r\n");
    head.into_bytes()
}

fn is_rejection(event: &Event, code: u16) -> bool {
    matches!(event, Event::Rejected(r) if r.status == code)
}

#[tokio::test]
async fn too_many_headers_get_431_and_a_log_line() {
    let mut l = start().await;
    let mut stream = TcpStream::connect(l.addr()).await.unwrap();
    let local = stream.local_addr().unwrap();
    stream.write_all(&post_with_headers(150)).await.unwrap();
    let response = read_to_close(&mut stream).await;
    assert_eq!(status(&response), 431, "{response}");
    let event = l.next_event(|e| is_rejection(e, 431)).await;
    assert_eq!(
        event.to_string(),
        format!("{local} rejected 431 too many header fields")
    );
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn exactly_100_headers_are_accepted() {
    let l = start().await;
    let response = raw(l.addr(), &post_with_headers(100)).await;
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(l.store.snapshot()[0].headers.len(), 100);
}

#[tokio::test]
async fn header_bytes_over_64_kib_get_431() {
    let mut l = start().await;
    let value = "a".repeat(70 * 1024);
    let request = format!("POST /h HTTP/1.1\r\nHost: x\r\nX-Big: {value}\r\n\r\n");
    let response = raw(l.addr(), request.as_bytes()).await;
    assert_eq!(status(&response), 431, "{response}");
    let event = l.next_event(|e| is_rejection(e, 431)).await;
    assert!(
        event.to_string().ends_with("header fields too large"),
        "{event}"
    );
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn h2_header_bytes_over_64_kib_get_431_and_a_log_line() {
    let mut l = start().await;
    let tcp = TcpStream::connect(l.addr()).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tcp),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let request = http::Request::post(format!("http://{}/h2", l.addr()))
        .header("x-big", "a".repeat(70_000))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 431);
    let event = l.next_event(|e| is_rejection(e, 431)).await;
    assert!(
        event
            .to_string()
            .ends_with("rejected 431 header fields too large"),
        "{event}"
    );
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn a_declared_oversize_body_gets_413_without_being_sent() {
    let mut l = start().await;
    let response = raw(
        l.addr(),
        b"POST /h HTTP/1.1\r\nHost: x\r\nContent-Length: 20971520\r\n\r\n",
    )
    .await;
    assert_eq!(status(&response), 413, "{response}");
    assert!(body(&response).contains("body larger than 10485760 bytes"));
    l.next_event(|e| is_rejection(e, 413)).await;
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn a_streamed_oversize_body_gets_413_and_the_connection_closes() {
    let mut l = start().await;
    let stream = TcpStream::connect(l.addr()).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();
    let sender = tokio::spawn(async move {
        let head = b"POST /h HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
        writer.write_all(head).await?;
        let chunk = format!("{:x}\r\n{}\r\n", 64 * 1024, "a".repeat(64 * 1024));
        // 11 MiB in 64 KiB chunks; the server stops reading at 10 MiB.
        for _ in 0..176 {
            writer.write_all(chunk.as_bytes()).await?;
        }
        writer.write_all(b"0\r\n\r\n").await
    });
    let mut out = Vec::new();
    let read = async {
        let mut buf = [0u8; 4096];
        while let Ok(n) = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    };
    tokio::time::timeout(common::SOON, read)
        .await
        .expect("connection not closed");
    let response = String::from_utf8_lossy(&out);
    assert_eq!(status(&response), 413, "{response}");
    assert_eq!(common::header(&response, "connection"), Some("close"));
    let _ = sender.await;
    l.next_event(|e| is_rejection(e, 413)).await;
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn transfer_encoding_with_content_length_is_400_either_order() {
    let mut l = start().await;
    for head in [
        "Transfer-Encoding: chunked\r\nContent-Length: 10",
        "Content-Length: 10\r\nTransfer-Encoding: chunked",
    ] {
        let request = format!("POST /h HTTP/1.1\r\nHost: x\r\n{head}\r\n\r\n3\r\nabc\r\n0\r\n\r\n");
        let response = raw(l.addr(), request.as_bytes()).await;
        assert_eq!(status(&response), 400, "{head}: {response}");
        l.next_event(|e| is_rejection(e, 400)).await;
    }
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn transfer_encoding_with_many_content_lengths_is_still_400() {
    let mut l = start().await;
    let mut request = String::from("POST /b HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n");
    request.push_str(&"Content-Length: 3\r\n".repeat(12));
    request.push_str("\r\n3\r\nabc\r\n0\r\n\r\n");
    let response = raw(l.addr(), request.as_bytes()).await;
    assert_eq!(status(&response), 400, "{response}");
    let event = l.next_event(|e| is_rejection(e, 400)).await;
    assert!(
        event
            .to_string()
            .ends_with("both Transfer-Encoding and Content-Length"),
        "{event}"
    );
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn garbage_request_lines_and_conflicting_lengths_are_400() {
    let mut l = start().await;
    for request in [
        &b"GARBAGE\r\n\r\n"[..],
        b"POST /h HTTP/1.1 extra\r\nHost: x\r\n\r\n",
        b"POST /h HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nabcdef",
    ] {
        let response = raw(l.addr(), request).await;
        assert_eq!(status(&response), 400, "{response}");
        let event = l.next_event(|e| is_rejection(e, 400)).await;
        assert!(
            event.to_string().contains("rejected 400 malformed request"),
            "{event}"
        );
    }
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn slow_headers_get_408_and_close_without_a_capture() {
    let limits = Limits {
        header_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let mut l = start_with(limits, Mocks::empty()).await;
    let mut stream = TcpStream::connect(l.addr()).await.unwrap();
    stream
        .write_all(b"POST /h HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let response = read_to_close(&mut stream).await;
    // The 408 the log reports is the one on the wire.
    assert_eq!(status(&response), 408, "{response}");
    assert!(
        body(&response).contains("request header timeout"),
        "{response}"
    );
    let event = l.next_event(|e| is_rejection(e, 408)).await;
    assert!(event.to_string().ends_with("request header timeout"));
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn a_slow_body_gets_408() {
    let limits = Limits {
        body_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let mut l = start_with(limits, Mocks::empty()).await;
    let mut stream = TcpStream::connect(l.addr()).await.unwrap();
    stream
        .write_all(b"POST /h HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc")
        .await
        .unwrap();
    let response = read_to_close(&mut stream).await;
    assert_eq!(status(&response), 408, "{response}");
    l.next_event(|e| is_rejection(e, 408)).await;
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn a_connection_that_sends_nothing_closes_at_the_header_timeout() {
    let limits = Limits {
        header_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let mut l = start_with(limits, Mocks::empty()).await;
    let mut stream = TcpStream::connect(l.addr()).await.unwrap();
    let started = std::time::Instant::now();
    let response = read_to_close(&mut stream).await;
    assert!(response.is_empty(), "{response}");
    // Well before the 60 s idle timeout.
    assert!(started.elapsed() < Duration::from_secs(30));
    // Nothing was asked, so nothing is logged.
    assert!(l.events.try_recv().is_err());
    assert!(l.store.is_empty());
}

#[tokio::test]
async fn connections_past_the_limit_wait_for_a_free_slot() {
    let limits = Limits {
        max_connections: 1,
        ..Limits::default()
    };
    let mocks = Mocks::empty();
    let hold = "mocks:\n- {path: /hold, delay_ms: 4000}\n";
    mocks.replace(parse(Path::new("mocks.yaml"), hold).unwrap());
    let mut l = start_with(limits, mocks).await;
    let mut holder = TcpStream::connect(l.addr()).await.unwrap();
    holder
        .write_all(b"POST /hold HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    // Its capture proves the first connection holds the only slot, which
    // it keeps for the mock's delay.
    l.next_event(|e| matches!(e, Event::Captured(_))).await;
    let mut waiting = TcpStream::connect(l.addr()).await.unwrap();
    waiting
        .write_all(b"POST /h HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0u8; 1];
    let early = tokio::time::timeout(Duration::from_millis(500), waiting.peek(&mut first)).await;
    assert!(early.is_err(), "answered while the only slot was taken");
    assert_eq!(status(&read_to_close(&mut holder).await), 200);
    let response = read_to_close(&mut waiting).await;
    assert_eq!(status(&response), 200, "{response}");
}
