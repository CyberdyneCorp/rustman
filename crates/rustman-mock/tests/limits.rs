//! Task 1.3: request limits over raw sockets: 413 past the body limit,
//! 431 past the header count or size, each as problem+json, and the limits
//! read from `[mock]`.

mod common;

use std::fmt::Write as _;

use common::{body, config, header, raw, start, start_with, status};
use rustman_mock::{MockConfig, MockSettings};

fn get_with_headers(count: usize) -> Vec<u8> {
    // Host and Connection are two of the `count` fields.
    let mut head = String::from("GET /users/me HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    for i in 2..count {
        let _ = write!(head, "X-H{i}: v\r\n");
    }
    head.push_str("\r\n");
    head.into_bytes()
}

fn assert_problem(response: &str, code: u16) {
    assert_eq!(status(response), code, "{response}");
    assert_eq!(
        header(response, "content-type"),
        Some("application/problem+json")
    );
    let problem: serde_json::Value = serde_json::from_str(body(response)).unwrap();
    assert_eq!(problem["status"], code);
}

#[tokio::test]
async fn oversized_body() {
    let mock = start().await;
    let request = "POST /pets HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 20971520\r\n\r\n";
    let response = raw(mock.local_addr(), request.as_bytes()).await;
    assert_problem(&response, 413);
    mock.stop().await;
}

#[tokio::test]
async fn a_streamed_body_past_the_limit_gets_413() {
    let mock = start_with(MockConfig {
        limits: rustman_webhook::Limits {
            max_body: 1024,
            ..rustman_webhook::Limits::default()
        },
        ..config()
    })
    .await;
    let chunk = "x".repeat(2048);
    let request = format!(
        "POST /pets HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{chunk}\r\n0\r\n\r\n",
        chunk.len()
    );
    let response = raw(mock.local_addr(), request.as_bytes()).await;
    assert_problem(&response, 413);
    mock.stop().await;
}

#[tokio::test]
async fn too_many_headers_get_431_and_exactly_100_pass() {
    let mock = start().await;
    assert_problem(&raw(mock.local_addr(), &get_with_headers(150)).await, 431);
    let response = raw(mock.local_addr(), &get_with_headers(100)).await;
    assert_eq!(status(&response), 200, "{response}");
    mock.stop().await;
}

#[tokio::test]
async fn header_bytes_over_64_kib_get_431() {
    let mock = start().await;
    let value = "a".repeat(70 * 1024);
    let request = format!("GET /users/me HTTP/1.1\r\nHost: x\r\nX-Big: {value}\r\n\r\n");
    assert_problem(&raw(mock.local_addr(), request.as_bytes()).await, 431);
    mock.stop().await;
}

#[tokio::test]
async fn limits_come_from_the_mock_section() {
    let settings = MockSettings::parse(
        "[mock]\nmax_body_bytes = 10\nmax_headers = 3\nmax_header_bytes = 200\n",
    )
    .unwrap();
    let mock = start_with(MockConfig {
        port: 0,
        ..settings.config()
    })
    .await;
    let addr = mock.local_addr();
    let small = "POST /pets HTTP/1.1\r\nHost: x\r\nContent-Length: 11\r\n\r\n01234567890";
    assert_problem(&raw(addr, small.as_bytes()).await, 413);
    assert_problem(&raw(addr, &get_with_headers(4)).await, 431);
    let wide = format!(
        "GET /users/me HTTP/1.1\r\nHost: x\r\nX-W: {}\r\n\r\n",
        "w".repeat(300)
    );
    assert_problem(&raw(addr, wide.as_bytes()).await, 431);
    mock.stop().await;
}
