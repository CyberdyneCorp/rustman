//! Tasks 7.1 and 7.2: forwarding unmatched requests to one upstream and
//! recording the proxied exchanges.

mod common;

use std::time::Duration;

use common::{SOON, config, get, header, json, send, start_with, status};
use rustman_core::SecretBackend;
use rustman_mock::{MockConfig, MockEvent, MockServer, Recorder, Upstream};
use rustman_test_support::TestServer;
use tokio::net::TcpListener;

fn proxied(upstream: &str) -> MockConfig {
    MockConfig {
        proxy_unmatched: Some(Upstream::parse(upstream).unwrap()),
        ..config()
    }
}

#[tokio::test]
async fn forward_unknown_route() {
    let upstream = TestServer::http().await;
    // Another origin the upstream redirects to: it must never be contacted.
    let other = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_url = format!("http://{}/stolen", other.local_addr().unwrap());
    let mock = start_with(proxied(&upstream.url("/"))).await;
    let addr = mock.local_addr();

    let echoed = get(
        addr,
        "/echo?x=1",
        &["X-Trace: t1", "Connection: close, X-Hop", "X-Hop: 1"],
    )
    .await;
    assert_eq!(status(&echoed), 200, "{echoed}");
    let body = json(&echoed);
    assert_eq!(body["path"], "/echo?x=1");
    let headers: Vec<(String, String)> = serde_json::from_value(body["headers"].clone()).unwrap();
    let sent = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(sent("x-trace").as_deref(), Some("t1"));
    assert_eq!(sent("x-hop"), None, "{headers:?}");
    let host = sent("host").unwrap();
    assert!(host.ends_with(&format!(":{}", upstream.port())), "{host}");

    // Documented operations are still answered by the mock.
    assert_eq!(json(&get(addr, "/users/me", &[]).await)["id"], "me");

    // The upstream's response comes back as it is, redirects included.
    let path = format!("/redirect/302?to={other_url}");
    let redirect = get(addr, &path, &[]).await;
    assert_eq!(status(&redirect), 302, "{redirect}");
    assert_eq!(header(&redirect, "location"), Some(other_url.as_str()));
    let contacted = tokio::time::timeout(Duration::from_millis(300), other.accept()).await;
    assert!(contacted.is_err(), "another origin was contacted");
    mock.stop().await;
}

#[tokio::test]
async fn proxied_responses_get_no_cors_and_no_proxy_credential() {
    let upstream = TestServer::http().await;
    let mock = start_with(MockConfig {
        cors: Some(true),
        ..proxied(&upstream.url("/"))
    })
    .await;
    let addr = mock.local_addr();
    let origin = "Origin: https://evil.example";
    let echoed = get(
        addr,
        "/echo",
        &[origin, "Proxy-Authorization: Basic cHJveHk6cHc="],
    )
    .await;
    assert_eq!(status(&echoed), 200, "{echoed}");
    assert_eq!(
        header(&echoed, "access-control-allow-origin"),
        None,
        "{echoed}"
    );
    assert_eq!(header(&echoed, "access-control-allow-credentials"), None);
    let headers: Vec<(String, String)> =
        serde_json::from_value(json(&echoed)["headers"].clone()).unwrap();
    assert!(
        headers.iter().all(|(n, _)| n != "proxy-authorization"),
        "{headers:?}"
    );

    // The mock's own responses echo the origin, without credentials.
    let own = get(addr, "/users/me", &[origin]).await;
    assert_eq!(
        header(&own, "access-control-allow-origin"),
        Some("https://evil.example")
    );
    assert_eq!(header(&own, "access-control-allow-credentials"), None);
    mock.stop().await;
}

#[tokio::test]
async fn an_unreachable_upstream_is_a_502() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let mock = start_with(proxied(&url)).await;
    let response = get(mock.local_addr(), "/legacy/report", &[]).await;
    assert_eq!(status(&response), 502, "{response}");
    assert_eq!(json(&response)["title"], "Upstream unreachable");
    mock.stop().await;
}

/// A workspace with one environment, `dev`.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"w\"\nversion = 1\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("environments")).unwrap();
    std::fs::write(
        dir.path().join("environments").join("dev.yaml"),
        "variables: {}\n",
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_request_file() {
    let upstream = TestServer::http().await;
    let ws = workspace();
    let recorder = Recorder::new(
        ws.path(),
        "recorded/",
        Some("dev".into()),
        SecretBackend::File,
    )
    .unwrap();
    let server = MockServer::bind(&proxied(&upstream.url("/")), common::pets().await)
        .await
        .unwrap()
        .with_recorder(recorder);
    let mut events = server.subscribe();
    let mock = server.spawn();
    let token = "Bearer tok-7f3c9a1e5b2d8c4f";
    let auth = format!("Authorization: {token}");
    for _ in 0..2 {
        let response = get(mock.local_addr(), "/legacy/report", &[&auth]).await;
        assert_eq!(status(&response), 404, "{response}");
    }
    let recorded = loop {
        let event = tokio::time::timeout(SOON, events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            MockEvent::Recorded(path) => break path,
            MockEvent::RecordFailed(message) => panic!("{message}"),
            _ => {}
        }
    };
    assert_eq!(recorded, "recorded/get-legacy-report.rman.yaml");
    let file = ws
        .path()
        .join("recorded")
        .join("get-legacy-report.rman.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("{{"), "{text}");
    assert!(!text.contains("tok-7f3c9a1e5b2d8c4f"), "{text}");
    assert!(text.contains(&upstream.url("/legacy/report")), "{text}");
    // The second, identical exchange was not recorded again.
    let files: Vec<_> = std::fs::read_dir(ws.path().join("recorded"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rman.yaml"))
        .collect();
    assert_eq!(files, ["get-legacy-report.rman.yaml"]);
    // Nothing in the workspace's tracked files holds the token.
    for entry in walk(ws.path()) {
        if entry.components().any(|c| c.as_os_str() == ".rustman") {
            continue;
        }
        let content = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(
            !content.contains("tok-7f3c9a1e5b2d8c4f"),
            "{}",
            entry.display()
        );
    }
    mock.stop().await;
}

/// Canaries for each credential channel a recorded request can carry.
const CANARIES: [&str; 6] = [
    "key-c4n4ry-1",
    "ck-c4n4ry-2",
    "qk-c4n4ry-3",
    "qt-c4n4ry-4",
    "pw-c4n4ry-5",
    "cs-c4n4ry-6",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recording_keeps_no_credential_of_any_channel() {
    let upstream = TestServer::http().await;
    let ws = workspace();
    let recorder = Recorder::new(
        ws.path(),
        "recorded/",
        Some("dev".into()),
        SecretBackend::File,
    )
    .unwrap();
    let server = MockServer::bind(&proxied(&upstream.url("/")), common::pets().await)
        .await
        .unwrap()
        .with_recorder(recorder);
    let mut events = server.subscribe();
    let mock = server.spawn();
    let [key, cookie, query_key, query_token, password, client_secret] = CANARIES;
    let path = format!("/legacy/login?api_key={query_key}&token={query_token}");
    let body =
        format!(r#"{{"user":"u","password":"{password}","client_secret":"{client_secret}"}}"#);
    let api_key = format!("X-Api-Key: {key}");
    let session = format!("Cookie: session={cookie}");
    let headers = [
        api_key.as_str(),
        session.as_str(),
        "Proxy-Authorization: Basic cHJveHk6cHc=",
        "Content-Type: application/json",
    ];
    send(mock.local_addr(), "POST", &path, &headers, &body).await;
    loop {
        let event = tokio::time::timeout(SOON, events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            MockEvent::Recorded(_) => break,
            MockEvent::RecordFailed(message) => panic!("{message}"),
            _ => {}
        }
    }
    let file = ws
        .path()
        .join("recorded")
        .join("post-legacy-login.rman.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("{{"), "{text}");
    assert!(
        !text.to_ascii_lowercase().contains("proxy-authorization"),
        "{text}"
    );
    assert!(!text.contains("cHJveHk6cHc="), "{text}");
    for entry in walk(ws.path()) {
        if entry.components().any(|c| c.as_os_str() == ".rustman") {
            continue;
        }
        let content = std::fs::read_to_string(&entry).unwrap_or_default();
        for canary in CANARIES {
            assert!(!content.contains(canary), "{canary} in {}", entry.display());
        }
    }
    mock.stop().await;
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
