//! Cookie jars per environment (task 4.7).

mod support;

use std::path::Path;

use rustman_core::model::RequestSettings;
use rustman_core::vars::ResolvedRequest;
use rustman_http::{CookieJar, HttpEngine, SendOptions};
use support::TestServer;
use support::request::{echoed_header, get, pair};

fn options(request: &ResolvedRequest, jar: &CookieJar) -> SendOptions {
    SendOptions::default().with_cookie_jar(&request.settings, jar)
}

async fn send(request: &ResolvedRequest, jar: &CookieJar) -> serde_json::Value {
    let exchange = HttpEngine::new()
        .send(request, &options(request, jar))
        .await
        .unwrap();
    serde_json::from_slice(&exchange.body).unwrap_or_default()
}

async fn login(server: &TestServer, root: &Path, env: &str) {
    let jar = CookieJar::for_environment(root, Some(env)).unwrap();
    let mut request = get(&server.url("/login"));
    request.method = "POST".into();
    send(&request, &jar).await;
}

#[tokio::test]
async fn login_then_authenticated_call() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    login(&server, dir.path(), "staging").await;
    // A new jar and a new engine: the cookie comes from the file, as in a
    // later run.
    let jar = CookieJar::for_environment(dir.path(), Some("staging")).unwrap();
    let echo = send(&get(&server.url("/echo")), &jar).await;
    assert_eq!(echoed_header(&echo, "cookie"), ["sid=abc"]);
    assert!(dir.path().join(".rustman/cookies/staging.json").is_file());
}

#[tokio::test]
async fn jars_are_isolated_per_environment() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    login(&server, dir.path(), "staging").await;
    let prod = CookieJar::for_environment(dir.path(), Some("prod")).unwrap();
    let echo = send(&get(&server.url("/echo")), &prod).await;
    assert!(echoed_header(&echo, "cookie").is_empty());
}

#[tokio::test]
async fn cookies_false_neither_sends_nor_stores() {
    let server = TestServer::http().await;
    let jar = CookieJar::in_memory();
    let off = RequestSettings {
        cookies: Some(false),
        ..RequestSettings::default()
    };
    let mut login = get(&server.url("/login"));
    login.settings = off.clone();
    send(&login, &jar).await;
    assert!(jar.cookies_for(&server.url("/")).is_empty());

    send(&get(&server.url("/login")), &jar).await;
    let mut me = get(&server.url("/echo"));
    me.settings = off;
    let echo = send(&me, &jar).await;
    assert!(echoed_header(&echo, "cookie").is_empty());
}

#[tokio::test]
async fn an_explicit_cookie_header_replaces_the_jar() {
    let server = TestServer::http().await;
    let jar = CookieJar::in_memory();
    send(&get(&server.url("/login")), &jar).await;
    let mut request = get(&server.url("/echo"));
    request.headers.push(pair("Cookie", "mine=1"));
    let echo = send(&request, &jar).await;
    assert_eq!(echoed_header(&echo, "cookie"), ["mine=1"]);
}

#[tokio::test]
async fn cookies_set_before_a_redirect_reach_the_next_hop() {
    let server = TestServer::http().await;
    let jar = CookieJar::in_memory();
    send(&get(&server.url("/login")), &jar).await;
    let request = get(&server.url("/redirect/302?to=/echo"));
    let echo = send(&request, &jar).await;
    assert_eq!(echoed_header(&echo, "cookie"), ["sid=abc"]);
}
