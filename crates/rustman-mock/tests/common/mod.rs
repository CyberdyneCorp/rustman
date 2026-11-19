//! A mock of `fixtures/pets.yaml` on a free loopback port and a raw TCP
//! client, so tests send exactly the bytes they mean.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustman_mock::{MockConfig, MockHandle, MockServer, MockState, Overrides, load};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Generous bound for anything that should happen "soon".
pub const SOON: Duration = Duration::from_secs(20);

pub fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    path.to_string_lossy().into_owned()
}

pub async fn pets() -> Arc<MockState> {
    Arc::new(load(&fixture("pets.yaml"), false).await.unwrap())
}

pub fn config() -> MockConfig {
    MockConfig {
        port: 0,
        ..MockConfig::default()
    }
}

pub async fn start_with(config: MockConfig) -> MockHandle {
    MockServer::bind(&config, pets().await)
        .await
        .unwrap()
        .spawn()
}

pub async fn start() -> MockHandle {
    start_with(config()).await
}

/// A mock of fixture `name` with the overrides of `mocks` (a directory).
pub async fn start_fixture(
    name: &str,
    mocks: Option<&std::path::Path>,
    config: MockConfig,
) -> MockHandle {
    let mut state = load(&fixture(name), false).await.unwrap();
    if let Some(dir) = mocks {
        state = state.with_overrides(Overrides::load_dir(dir).unwrap());
    }
    MockServer::bind(&config, Arc::new(state))
        .await
        .unwrap()
        .spawn()
}

/// A directory holding one mock file with `text`.
pub fn mocks_dir(text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.rman-mock.yaml"), text).unwrap();
    dir
}

/// `method path` with extra header lines and a body, on a connection
/// closed after it.
pub async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[&str],
    body: &str,
) -> String {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: mock\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    head.push_str(body);
    raw(addr, head.as_bytes()).await
}

/// `send` with `Content-Type: application/json`.
pub async fn post_json(addr: SocketAddr, method: &str, path: &str, body: &str) -> String {
    send(
        addr,
        method,
        path,
        &["Content-Type: application/json"],
        body,
    )
    .await
}

/// Send `request` and read the reply until the server closes.
pub async fn raw(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request).await.unwrap();
    read_to_close(&mut stream).await
}

/// Everything the server sends until it closes (a reset ends it too).
pub async fn read_to_close(stream: &mut TcpStream) -> String {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    let read = async {
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
    };
    tokio::time::timeout(SOON, read)
        .await
        .expect("server did not close");
    String::from_utf8_lossy(&out).into_owned()
}

/// `GET <path>` with extra header lines, on a connection closed after it.
pub async fn get(addr: SocketAddr, path: &str, headers: &[&str]) -> String {
    request(addr, "GET", path, headers).await
}

pub async fn request(addr: SocketAddr, method: &str, path: &str, headers: &[&str]) -> String {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: mock\r\nConnection: close\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    raw(addr, head.as_bytes()).await
}

/// The status code of a raw HTTP/1 response.
pub fn status(response: &str) -> u16 {
    response
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("not a response: {response:?}"))
}

/// The body of a raw HTTP/1 response with a `Content-Length`.
pub fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").map_or("", |(_, b)| b)
}

pub fn json(response: &str) -> serde_json::Value {
    serde_json::from_str(body(response)).unwrap_or_else(|e| panic!("{e}: {response}"))
}

/// The value of header `name` in a raw response.
pub fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    let head = response.split_once("\r\n\r\n").map_or(response, |(h, _)| h);
    head.lines().skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}
