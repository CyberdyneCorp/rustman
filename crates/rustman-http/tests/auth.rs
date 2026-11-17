//! Basic, bearer and API-key auth (task 4.8).

mod support;

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::model::{ApiKeyLocation, Auth};
use rustman_core::vars::{NoSecrets, ResolvedRequest, Resolver, resolve_request};
use rustman_http::{HttpEngine, SendOptions};
use support::TestServer;
use support::request::{echoed_header, get, pair};

async fn echo(request: &ResolvedRequest) -> serde_json::Value {
    let exchange = HttpEngine::new()
        .send(request, &SendOptions::default())
        .await
        .unwrap();
    serde_json::from_slice(&exchange.body).unwrap()
}

#[tokio::test]
async fn basic_auth_sends_the_encoded_credentials() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.auth = Some(Auth::Basic {
        username: "ana".into(),
        password: "pw".into(),
    });
    let echo = echo(&request).await;
    assert_eq!(echoed_header(&echo, "authorization"), ["Basic YW5hOnB3"]);
}

#[tokio::test]
async fn an_echoed_basic_credential_holding_a_secret_is_redacted() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.auth = Some(Auth::Basic {
        username: "u".into(),
        password: "CANARY123".into(),
    });
    request.secrets = vec![rustman_core::Secret::new("pw", "CANARY123".to_owned())];
    let exchange = HttpEngine::new()
        .send(&request, &SendOptions::default())
        .await
        .unwrap();
    // `dTpDQU5BUlkxMjM=` is base64 of `u:CANARY123`.
    let raw = String::from_utf8_lossy(&exchange.body).into_owned();
    assert!(raw.contains("dTpDQU5BUlkxMjM="));
    let redacted = rustman_core::Redactor::for_request(&request)
        .text(&raw)
        .into_owned();
    assert!(!redacted.contains("dTpDQU5BUlkxMjM="), "{redacted}");
    assert!(redacted.contains("Basic «redacted:pw»"), "{redacted}");
}

#[tokio::test]
async fn bearer_auth_sends_the_token() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.auth = Some(Auth::Bearer {
        token: "t0k".into(),
    });
    let echo = echo(&request).await;
    assert_eq!(echoed_header(&echo, "authorization"), ["Bearer t0k"]);
}

#[tokio::test]
async fn an_explicit_authorization_header_wins() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.headers.push(pair("Authorization", "Token mine"));
    request.auth = Some(Auth::Bearer {
        token: "t0k".into(),
    });
    let echo = echo(&request).await;
    assert_eq!(echoed_header(&echo, "authorization"), ["Token mine"]);
}

#[tokio::test]
async fn api_key_in_a_header() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.auth = Some(Auth::ApiKey {
        name: "X-Api-Key".into(),
        value: "k1".into(),
        location: Some(ApiKeyLocation::Header),
    });
    let echo = echo(&request).await;
    assert_eq!(echoed_header(&echo, "x-api-key"), ["k1"]);
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The scenario as written: `value: "{{k}}"` with `k=123` in the
/// environment, interpolated before the key is placed in the query.
#[tokio::test]
async fn api_key_in_the_query_is_interpolated_first() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        "variables:\n  k: \"123\"\n",
    );
    let request_file = format!(
        "version: 1\nkind: http\nname: Keyed\nmethod: GET\nurl: '{}'\n\
         auth: {{type: api_key, name: key, value: \"{{{{k}}}}\", in: query}}\n",
        server.url("/echo?a=1")
    );
    write(dir.path(), "keyed.rman.yaml", &request_file);
    let ws = Workspace::load(dir.path()).unwrap();
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let target = ws.find_by_name("Keyed").remove(0);
    let resolver = Resolver::for_request(&ws.settings, &target, env, &NoSecrets);
    let request = resolve_request(&target, &resolver).unwrap();
    let echo = echo(&request).await;
    assert_eq!(echo["path"], "/echo?a=1&key=123");
}
