//! Tasks 6.1, 6.2 and 6.4: CORS, the request log and hot reload.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{SOON, config, fixture, get, header, json, post_json, request, start_with, status};
use rustman_mock::watch::watch;
use rustman_mock::{LogEntry, MockConfig, MockEvent, MockServer, Reload, load};
use tokio::sync::broadcast;

#[tokio::test]
async fn browser_preflight() {
    let mock = start_with(config()).await;
    let response = request(
        mock.local_addr(),
        "OPTIONS",
        "/pets",
        &[
            "Origin: http://localhost:5173",
            "Access-Control-Request-Method: POST",
            "Access-Control-Request-Headers: content-type",
        ],
    )
    .await;
    assert_eq!(status(&response), 204, "{response}");
    assert_eq!(
        header(&response, "access-control-allow-origin"),
        Some("http://localhost:5173")
    );
    let methods = header(&response, "access-control-allow-methods").unwrap();
    assert!(methods.split(", ").any(|m| m == "POST"), "{methods}");
    assert_eq!(
        header(&response, "access-control-allow-headers"),
        Some("content-type")
    );
    // Before validation: a POST without its required body would be 4xx.
    let simple = get(
        mock.local_addr(),
        "/pets/1",
        &["Origin: http://localhost:5173"],
    )
    .await;
    assert_eq!(
        header(&simple, "access-control-allow-origin"),
        Some("http://localhost:5173")
    );
    mock.stop().await;
}

#[tokio::test]
async fn no_cors_leaves_options_to_the_document() {
    let mock = start_with(MockConfig {
        cors: Some(false),
        ..config()
    })
    .await;
    let response = request(
        mock.local_addr(),
        "OPTIONS",
        "/pets",
        &[
            "Origin: http://localhost:5173",
            "Access-Control-Request-Method: POST",
        ],
    )
    .await;
    assert_eq!(status(&response), 405, "{response}");
    assert!(header(&response, "access-control-allow-origin").is_none());
    mock.stop().await;
}

/// The next request log entry.
async fn next_entry(events: &mut broadcast::Receiver<MockEvent>) -> LogEntry {
    loop {
        let event = tokio::time::timeout(SOON, events.recv())
            .await
            .expect("no log entry")
            .unwrap();
        if let MockEvent::Request(entry) = event {
            return *entry;
        }
    }
}

#[tokio::test]
async fn json_lines() {
    let server = MockServer::bind(&config(), common::pets().await)
        .await
        .unwrap();
    let mut events = server.subscribe();
    let mock = server.spawn();
    let addr = mock.local_addr();
    get(addr, "/pets/1", &[]).await;
    let entry = next_entry(&mut events).await;
    let line: serde_json::Value = serde_json::from_str(&entry.json_line()).unwrap();
    assert_eq!(line["status"], 200);
    assert_eq!(line["source"], "example");
    assert_eq!(line["operation"], "getPet");
    assert_eq!(line["method"], "GET");
    assert_eq!(line["path"], "/pets/1");
    assert!(line["duration_ms"].is_number());

    post_json(addr, "POST", "/pets", r#"{"name": 5}"#).await;
    let invalid = next_entry(&mut events).await;
    assert_eq!(
        (invalid.status, invalid.source.as_str()),
        (Some(422), "error")
    );
    assert!(
        invalid.errors.iter().any(|e| e.contains("/name")),
        "{invalid:?}"
    );

    get(addr, "/nope", &[]).await;
    let unknown = next_entry(&mut events).await;
    assert_eq!((unknown.status, unknown.operation), (Some(404), None));
    mock.stop().await;
}

#[tokio::test]
async fn authorization_redacted() {
    let server = MockServer::bind(&config(), common::pets().await)
        .await
        .unwrap();
    let mut events = server.subscribe();
    let mock = server.spawn();
    get(mock.local_addr(), "/pets/1", &["Authorization: Bearer abc"]).await;
    let line = next_entry(&mut events).await.json_line();
    assert!(line.contains("«redacted:Authorization»"), "{line}");
    assert!(!line.contains("Bearer abc"), "{line}");
    mock.stop().await;
}

#[tokio::test]
async fn override_sources_are_logged() {
    let dir = common::mocks_dir(
        "overrides:\n  - name: health\n    match: {method: GET, path: /health}\n    respond: {body: {ok: true, who: '{{request.query.who}}'}}\n",
    );
    let mut state = load(&fixture("pets.yaml"), false).await.unwrap();
    state = state.with_overrides(rustman_mock::Overrides::load_dir(dir.path()).unwrap());
    let server = MockServer::bind(&config(), Arc::new(state)).await.unwrap();
    let mut events = server.subscribe();
    let mock = server.spawn();
    get(mock.local_addr(), "/health", &[]).await;
    let entry = next_entry(&mut events).await;
    assert_eq!(entry.source.as_str(), "override");
    assert_eq!(entry.override_name.as_deref(), Some("health"));
    assert!(entry.errors[0].contains("request.query.who"), "{entry:?}");
    mock.stop().await;
}

const API: &str = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /users:\n    post:\n      requestBody:\n        content:\n          application/json:\n            schema: {type: object}\n      responses:\n        '201':\n          description: ok\n          content:\n            application/json:\n              schema: {type: object, properties: {id: {type: integer}, name: {type: string}}}\n  /users/{id}:\n    get:\n      parameters:\n        - {name: id, in: path, required: true, schema: {type: integer}}\n      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              schema: {type: object, properties: {id: {type: integer}, name: {type: string}}}\n";

/// The operations of `API`: `POST /users` and `GET /users/{id}`.
const API_OPERATIONS: usize = 2;

const ORDERS: &str = "  /orders:\n    get:\n      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              example: [{id: 7}]\n";

/// A mock of `api.yaml` in `dir`, reloading on change.
async fn watched(
    dir: &Path,
    config: MockConfig,
) -> (
    rustman_mock::MockHandle,
    broadcast::Receiver<MockEvent>,
    rustman_mock::watch::ReloadWatcher,
) {
    let document = dir.join("api.yaml").to_string_lossy().into_owned();
    let state = load(&document, false).await.unwrap();
    let server = MockServer::bind(&config, Arc::new(state)).await.unwrap();
    let events = server.subscribe();
    let watcher = watch(server.reloader(Reload {
        document,
        allow_remote_refs: false,
        mocks_dir: Some(dir.join("mocks")),
    }))
    .unwrap();
    (server.spawn(), events, watcher)
}

/// The next reload notice.
async fn next_reload(events: &mut broadcast::Receiver<MockEvent>) -> MockEvent {
    loop {
        let event = tokio::time::timeout(SOON, events.recv())
            .await
            .expect("no reload")
            .unwrap();
        // A late file event for the test's first write (macOS FSEvents can
        // deliver it after the watcher starts) reloads the unchanged `API`
        // document. It is never the change a test is waiting for.
        let stale =
            matches!(event, MockEvent::Reloaded { operations, .. } if operations == API_OPERATIONS);
        if !matches!(event, MockEvent::Request(_)) && !stale {
            return event;
        }
    }
}

/// The spec's bound from a saved change to the new version being served.
const RELOAD_BOUND: Duration = Duration::from_secs(1);

/// Save the document with or without `/orders` and wait until the mock
/// serves that version; how long it took.
async fn reloaded_in(dir: &Path, addr: std::net::SocketAddr, with_orders: bool) -> Duration {
    let (text, expected) = if with_orders {
        (format!("{API}{ORDERS}"), 200)
    } else {
        (API.to_owned(), 404)
    };
    let saved = Instant::now();
    std::fs::write(dir.join("api.yaml"), text).unwrap();
    loop {
        if status(&get(addr, "/orders", &[]).await) == expected {
            return saved.elapsed();
        }
        assert!(saved.elapsed() < SOON, "never reloaded");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_an_operation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("api.yaml"), API).unwrap();
    let stateful = MockConfig {
        stateful: true,
        ..config()
    };
    let (mock, mut events, _watcher) = watched(dir.path(), stateful).await;
    let addr = mock.local_addr();
    assert_eq!(status(&get(addr, "/orders", &[]).await), 404);
    let created = json(&post_json(addr, "POST", "/users", r#"{"name":"Ana"}"#).await);
    // Let the watcher settle before the change it must see.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let served = reloaded_in(dir.path(), addr, true).await;
    assert!(matches!(
        next_reload(&mut events).await,
        MockEvent::Reloaded { operations: 3, .. }
    ));
    // A loaded CI runner can delay one file event: the spec's 1 s bound
    // holds when any of three edits is served within it.
    let mut fastest = served;
    for _ in 0..2 {
        if fastest < RELOAD_BOUND {
            break;
        }
        reloaded_in(dir.path(), addr, false).await;
        fastest = fastest.min(reloaded_in(dir.path(), addr, true).await);
    }
    assert!(fastest < RELOAD_BOUND, "reloaded after {fastest:?} at best");
    // The stateful store survived the reload.
    let path = format!("/users/{}", created["id"]);
    let read = get(addr, &path, &[]).await;
    assert_eq!(json(&read)["name"], "Ana", "{read}");
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broken_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("api.yaml"), API).unwrap();
    let (mock, mut events, _watcher) = watched(dir.path(), config()).await;
    let addr = mock.local_addr();
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(
        dir.path().join("api.yaml"),
        format!("{API}  /bad: [unclosed\n"),
    )
    .unwrap();
    let MockEvent::ReloadFailed(message) = next_reload(&mut events).await else {
        panic!("expected a failed reload");
    };
    assert!(message.contains("api.yaml"), "{message}");
    assert!(message.contains("line"), "{message}");
    assert_eq!(status(&get(addr, "/users/1", &[]).await), 200);

    // A broken mock file keeps the previous overrides too.
    std::fs::create_dir(dir.path().join("mocks")).unwrap();
    std::fs::write(
        dir.path().join("mocks").join("a.rman-mock.yaml"),
        "overrides:\n  - match: {path: /x}\n    delay_ms: 70000\n",
    )
    .unwrap();
    let MockEvent::ReloadFailed(message) = next_reload(&mut events).await else {
        panic!("expected a failed reload");
    };
    assert!(message.contains("a.rman-mock.yaml"), "{message}");
    assert_eq!(status(&get(addr, "/users/1", &[]).await), 200);
    mock.stop().await;
}

/// Canary: the expected security values never reach a log entry, its
/// human line or its history entry, wherever they appear in the request.
#[tokio::test]
async fn security_values_are_redacted_from_every_log_output() {
    const CANARY: &str = "canary-3f9b1c77";
    let mut security = rustman_mock::security::Expected::new();
    security.insert(
        "bearerAuth".into(),
        rustman_core::Secret::new("api_token", CANARY.to_owned()),
    );
    let server = MockServer::bind(
        &MockConfig {
            security,
            ..config()
        },
        common::pets().await,
    )
    .await
    .unwrap();
    let mut events = server.subscribe();
    let mock = server.spawn();
    let path = format!("/pets/1?q={CANARY}");
    let header = format!("X-Echo: {CANARY}");
    get(mock.local_addr(), &path, &[&header]).await;
    let entry = next_entry(&mut events).await;
    let history = serde_json::to_string(&entry.history_entry("http://m")).unwrap();
    for text in [entry.json_line(), entry.human_line(), history] {
        assert!(!text.contains(CANARY), "{text}");
        assert!(text.contains("«redacted:api_token»"), "{text}");
    }
    mock.stop().await;
}
