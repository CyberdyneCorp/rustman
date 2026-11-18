//! The Streamable HTTP transport (agent-mcp-interface task 1.4): no token
//! gives 401, a foreign Origin gives 403, and the right token with a
//! loopback or listed Origin reaches `tools/list`. Loopback only.

use std::path::Path;
use std::time::Duration;

use ::http::{Method, Request, StatusCode};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use rustman_core::safety::Origin;
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_mcp::server::audit::AUDIT_DIR;
use rustman_mcp::server::http::{self, HttpGuard};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "test-token-5d1e";

struct Served {
    url: String,
    root: tempfile::TempDir,
    cancel: CancellationToken,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn serve() -> Served {
    serve_with(false).await
}

async fn serve_with(allow_remote: bool) -> Served {
    let root = tempfile::tempdir().unwrap();
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n";
    std::fs::write(root.path().join("rustman.toml"), toml).unwrap();
    let ws = Workspace::load(root.path()).unwrap();
    let secrets = SecretStore::with_keychain(root.path(), None);
    let sessions = Sessions::new(ws, secrets, None);
    let listed = vec![Origin::parse("https://inspector.example.com").unwrap()];
    let guard = HttpGuard::new(TOKEN.to_owned(), listed);
    let cancel = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let any_host = http::accepts_any_host(local, allow_remote);
    let router = http::router(sessions, guard, any_host, cancel.clone());
    let url = format!("http://{local}/mcp");
    tokio::spawn(http::serve(listener, router, cancel.clone()));
    Served { url, root, cancel }
}

struct Reply {
    status: StatusCode,
    session: Option<String>,
    body: String,
}

async fn post(url: &str, headers: &[(&str, &str)], body: &Value) -> Reply {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let response = client.request(request).await.unwrap();
    let status = response.status();
    let session = response
        .headers()
        .get("mcp-session-id")
        .map(|v| v.to_str().unwrap().to_owned());
    let collected = tokio::time::timeout(Duration::from_secs(30), response.into_body().collect());
    let bytes = collected.await.unwrap().unwrap().to_bytes();
    Reply {
        status,
        session,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// The JSON-RPC message with `id` in a JSON or SSE body.
fn message(body: &str, id: u64) -> Value {
    let candidates = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .chain(std::iter::once(body.trim()));
    candidates
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("no message {id} in {body}"))
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "http-test", "version": "0"}
        }
    })
}

fn audit_written(root: &Path) -> bool {
    root.join(AUDIT_DIR).exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_a_token_gets_401_and_reaches_nothing() {
    let served = serve().await;
    let reply = post(&served.url, &[], &initialize()).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.session.is_none());
    let wrong = post(
        &served.url,
        &[("authorization", "Bearer nope")],
        &initialize(),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                      "params": {"name": "send_request", "arguments": {}}});
    assert_eq!(
        post(&served.url, &[], &call).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert!(!audit_written(served.root.path()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_origin_gets_403_even_with_the_token() {
    let served = serve().await;
    let bearer = format!("Bearer {TOKEN}");
    let headers = [
        ("authorization", bearer.as_str()),
        ("origin", "https://evil.example"),
    ];
    let reply = post(&served.url, &headers, &initialize()).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

async fn list_tools(served: &Served, origin: Option<&str>) -> Value {
    let bearer = format!("Bearer {TOKEN}");
    let mut headers = vec![("authorization", bearer.as_str())];
    headers.extend(origin.map(|o| ("origin", o)));
    let init = post(&served.url, &headers, &initialize()).await;
    assert_eq!(init.status, StatusCode::OK, "{}", init.body);
    assert_eq!(
        message(&init.body, 1)["result"]["serverInfo"]["name"],
        "rustman"
    );
    let session = init.session.expect("a session id");
    headers.push(("mcp-session-id", session.as_str()));
    headers.push(("mcp-protocol-version", "2025-06-18"));
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let ack = post(&served.url, &headers, &initialized).await;
    assert!(ack.status.is_success(), "{}", ack.status);
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let reply = post(&served.url, &headers, &list).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    message(&reply.body, 2)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_right_token_reaches_tools_list() {
    let served = serve().await;
    let tools = list_tools(&served, None).await;
    assert!(tools["result"]["tools"].is_array(), "{tools}");
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_and_listed_origins_reach_tools_list() {
    let served = serve().await;
    for origin in ["http://localhost:6274", "https://inspector.example.com"] {
        let tools = list_tools(&served, Some(origin)).await;
        assert!(tools["result"]["tools"].is_array(), "{origin}: {tools}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_host_is_refused_on_loopback_even_with_allow_remote() {
    let served = serve_with(true).await;
    let bearer = format!("Bearer {TOKEN}");
    let port = served
        .url
        .split(':')
        .nth(2)
        .unwrap()
        .trim_end_matches("/mcp");
    let host = format!("evil.example:{port}");
    let headers = [("authorization", bearer.as_str()), ("host", host.as_str())];
    let reply = post(&served.url, &headers, &initialize()).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    assert!(list_tools(&served, None).await["result"]["tools"].is_array());
}
