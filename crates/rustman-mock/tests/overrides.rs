//! Tasks 4.1-4.3: hand-written overrides, response templates, latency and
//! faults.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{body, config, get, header, json, mocks_dir, post_json, start_fixture, status};
use rustman_mock::MockConfig;
use serde_json::json;

async fn with_mocks(text: &str) -> (rustman_mock::MockHandle, tempfile::TempDir) {
    with_mocks_config(text, config()).await
}

async fn with_mocks_config(
    text: &str,
    config: MockConfig,
) -> (rustman_mock::MockHandle, tempfile::TempDir) {
    let dir = mocks_dir(text);
    let mock = start_fixture("pets.yaml", Some(dir.path()), config).await;
    (mock, dir)
}

#[tokio::test]
async fn override_by_body_field() {
    let (mock, _dir) = with_mocks(concat!(
        "overrides:\n",
        "  - match:\n",
        "      method: POST\n",
        "      path: /payments\n",
        "      body:\n",
        "        $.amount: {gt: 10000}\n",
        "    respond:\n",
        "      status: 402\n",
        "      body: {error: payment required}\n",
    ))
    .await;
    let addr = mock.local_addr();
    let big = post_json(addr, "POST", "/payments", r#"{"amount": 50000}"#).await;
    assert_eq!(status(&big), 402, "{big}");
    assert_eq!(json(&big), json!({"error": "payment required"}));
    let small = post_json(addr, "POST", "/payments", r#"{"amount": 50}"#).await;
    assert_eq!(status(&small), 404, "{small}");
    mock.stop().await;
}

#[tokio::test]
async fn override_for_undocumented_path() {
    let (mock, _dir) = with_mocks(
        "overrides:\n  - match: {method: GET, path: /health}\n    respond: {status: 200, body: '{\"ok\":true}'}\n",
    )
    .await;
    let response = get(mock.local_addr(), "/health", &[]).await;
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(body(&response), r#"{"ok":true}"#);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    mock.stop().await;
}

#[tokio::test]
async fn overrides_come_before_prefer_and_examples() {
    let (mock, _dir) = with_mocks(concat!(
        "overrides:\n",
        "  - match: {path: '/pets/{id}', headers: {X-Scenario: gone}}\n",
        "    respond: {status: 410, headers: {X-Mock: '{{request.path.id}}'}}\n",
        "  - match: {path: '/pets/*'}\n",
        "    respond: {body: {first: true}}\n",
        "  - match: {path: '/pets/*'}\n",
        "    respond: {body: {first: false}}\n",
    ))
    .await;
    let addr = mock.local_addr();
    let gone = get(addr, "/pets/7", &["X-Scenario: gone", "Prefer: code=404"]).await;
    assert_eq!(status(&gone), 410, "{gone}");
    assert_eq!(header(&gone, "x-mock"), Some("7"));
    let first = get(addr, "/pets/7", &["Prefer: code=404"]).await;
    assert_eq!(json(&first), json!({"first": true}));
    // Validation still runs first for documented operations.
    assert_eq!(status(&get(addr, "/pets/abc", &[]).await), 400);
    mock.stop().await;
}

#[tokio::test]
async fn echo_a_path_parameter() {
    let (mock, _dir) = with_mocks(
        "overrides:\n  - match: {method: GET, path: '/users/{id}'}\n    respond:\n      body: {id: '{{request.path.id}}'}\n",
    )
    .await;
    let response = get(mock.local_addr(), "/users/42", &[]).await;
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(json(&response), json!({"id": "42"}));
    mock.stop().await;
}

/// Everything logged through `tracing` while the guard lives.
fn capture_logs() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buffer);
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Sink(Arc::clone(&writer)))
        .finish();
    (buffer, tracing::subscriber::set_default(subscriber))
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn unresolved_references_render_empty_and_are_logged() {
    let (logs, _guard) = capture_logs();
    let (mock, _dir) = with_mocks(
        "overrides:\n  - name: echo\n    match: {path: /echo}\n    respond:\n      body: '[{{request.query.missing}}|{{request.query.q}}]'\n",
    )
    .await;
    let response = get(mock.local_addr(), "/echo?q=hi", &[]).await;
    assert_eq!(body(&response), "[|hi]");
    mock.stop().await;
    let logged = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains(
            "override echo: unresolved template reference(s) rendered empty: request.query.missing"
        ),
        "{logged}"
    );
}

#[tokio::test]
async fn delayed_response() {
    let (mock, _dir) = with_mocks(
        "overrides:\n  - match: {path: /slow}\n    delay_ms: 2000\n    respond: {body: ok}\n",
    )
    .await;
    // Warm the connection path first so only the delay is measured.
    let _ = get(mock.local_addr(), "/pets/1", &[]).await;
    let started = Instant::now();
    let response = get(mock.local_addr(), "/slow", &[]).await;
    assert!(
        started.elapsed() >= Duration::from_millis(2000),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(status(&response), 200);
    mock.stop().await;
}

#[tokio::test]
async fn error_rate() {
    let (mock, _dir) = with_mocks(
        "overrides:\n  - match: {path: /flaky}\n    fault: {error_rate: 1.0, status: 503}\n",
    )
    .await;
    let response = get(mock.local_addr(), "/flaky", &[]).await;
    assert_eq!(status(&response), 503, "{response}");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    assert_eq!(json(&response)["status"], 503);
    mock.stop().await;
}

#[tokio::test]
async fn dropped_connection() {
    let (mock, _dir) =
        with_mocks("overrides:\n  - match: {path: /drop}\n    fault: {drop_connection: true}\n")
            .await;
    let response = get(mock.local_addr(), "/drop", &[]).await;
    assert_eq!(response, "", "no response bytes expected");
    // The server keeps serving other connections.
    assert_eq!(status(&get(mock.local_addr(), "/pets/1", &[]).await), 200);
    mock.stop().await;
}

async fn fault_sequence(seed: u64) -> Vec<u16> {
    let (mock, _dir) = with_mocks_config(
        "overrides:\n  - match: {path: /flaky}\n    fault: {error_rate: 0.5}\n",
        MockConfig { seed, ..config() },
    )
    .await;
    let mut statuses = Vec::new();
    for _ in 0..24 {
        statuses.push(status(&get(mock.local_addr(), "/flaky", &[]).await));
        // Requests to other routes do not shift the sequence.
        let _ = get(mock.local_addr(), "/pets/1", &[]).await;
    }
    mock.stop().await;
    statuses
}

#[tokio::test]
async fn reproducible_fault_sequence() {
    let first = fault_sequence(3).await;
    assert_eq!(first, fault_sequence(3).await);
    assert!(first.contains(&500) && first.contains(&200), "{first:?}");
    assert_ne!(first, fault_sequence(4).await);
}

#[tokio::test]
async fn slow_body() {
    let (mock, _dir) = with_mocks(
        "overrides:\n  - match: {path: /trickle}\n    fault: {slow_body_bps: 40}\n    respond: {body: '0123456789012345678901234567890123456789'}\n",
    )
    .await;
    let started = Instant::now();
    let response = get(mock.local_addr(), "/trickle", &[]).await;
    // 40 bytes at 40 B/s: ten 4-byte chunks, 100 ms apart.
    assert!(
        started.elapsed() >= Duration::from_millis(850),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(body(&response), "0123456789012345678901234567890123456789");
    mock.stop().await;
}
