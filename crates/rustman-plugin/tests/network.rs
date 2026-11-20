//! Plugin network access (plugins spec, "Plugin network access"; task
//! 3.3): the host `http` import sends granted requests through the
//! GovernedSender and records them in history, redacted.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Ws, plugin, verified};
use rustman_core::Secret;
use rustman_core::history::HistoryStore;
use rustman_core::safety::{Governor, Limits, PolicyOptions, SafetyPolicy, SafetySettings};
use rustman_http::{GovernedSender, SendOptions};
use rustman_plugin::fixtures;
use rustman_plugin::sandbox::net::GovernedNetwork;
use rustman_plugin::sandbox::{HostContext, SystemServices};
use rustman_test_support::TestServer;

const API_TOKEN: &str = "tok-5f1c2a9e7b3d";

/// A context whose network allows the policy `target` and records
/// history in `history`.
fn context(root: &Path, target: &str, history: &Path) -> HostContext {
    let policy = SafetyPolicy::new(
        &SafetySettings::default(),
        PolicyOptions {
            target,
            ..PolicyOptions::default()
        },
    )
    .unwrap();
    let sender = GovernedSender::new(policy, Governor::new(Limits::default()));
    let secrets = vec![Secret::new("api_token", API_TOKEN.to_owned())];
    let base = HostContext::new(root).with_secrets(secrets.clone());
    let network = GovernedNetwork::new(
        Arc::new(sender),
        SendOptions::default(),
        Some(HistoryStore::at(history, 100)),
        base.redactor(),
    )
    .unwrap();
    let services = SystemServices {
        network: Some(network),
    };
    HostContext::new(root)
        .with_secrets(secrets)
        .with_services(Arc::new(services))
}

fn history_texts(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect()
}

async fn generate(
    plugin: rustman_plugin::Plugin,
    url: String,
) -> (
    rustman_plugin::Plugin,
    Result<String, rustman_plugin::CallError>,
) {
    tokio::task::spawn_blocking(move || {
        let result = plugin.generate("fetch", &[url]);
        (plugin, result)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn granted_host() {
    let server = TestServer::http().await;
    let ws = Ws::new();
    let history = ws.root().join(".rustman/history");
    let grant = format!("net:localhost:{}", server.port());
    let manifest = fixtures::manifest("auth-fetch", &["generator"], &["net", "secrets:read"]);
    let grants = [grant.as_str(), "secrets:read:api_token"];
    let verified = verified("auth-fetch", fixtures::fetcher(), &manifest, &grants);
    let plugin = plugin(&verified, context(ws.root(), &server.url("/"), &history));

    let (plugin, body) = generate(plugin, server.url("/echo")).await;

    // The request was sent, with the revealed secret on the wire.
    let body = body.unwrap();
    assert!(body.contains("\"path\":\"/echo\""), "{body}");
    assert!(body.contains(API_TOKEN), "{body}");
    assert!(plugin.take_denials().is_empty());
    // It is recorded in history like any other exchange, redacted.
    let entries = history_texts(&history);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].contains("plugin:auth-fetch"), "{}", entries[0]);
    assert!(entries[0].contains("/echo"), "{}", entries[0]);
    assert!(!entries[0].contains(API_TOKEN), "{}", entries[0]);
    assert!(
        entries[0].contains("«redacted:api_token»"),
        "{}",
        entries[0]
    );
}

#[tokio::test]
async fn other_host() {
    let server = TestServer::http().await;
    let ws = Ws::new();
    let history = ws.root().join(".rustman/history");
    let grant = format!("net:localhost:{}", server.port());
    let manifest = fixtures::manifest("auth-fetch", &["generator"], &["net"]);
    let verified = verified(
        "auth-fetch",
        fixtures::fetcher(),
        &manifest,
        &[grant.as_str()],
    );
    let plugin = plugin(&verified, context(ws.root(), &server.url("/"), &history));

    let (plugin, result) = generate(plugin, "https://evil.example.net/".to_owned()).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "plugin auth-fetch: net evil.example.net"
    );
    // The fetcher also tries to reveal `api_token`, which it may not.
    assert_eq!(
        plugin.take_denials(),
        [
            "capability_denied: secrets api_token",
            "capability_denied: net evil.example.net"
        ]
    );
    // Same host, another port: not granted either.
    let (_, result) = generate(plugin, "http://localhost:1/".to_owned()).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "plugin auth-fetch: net localhost"
    );
    assert!(history_texts(&history).is_empty());
}

#[tokio::test]
async fn the_safety_policy_still_applies() {
    let server = TestServer::http().await;
    let ws = Ws::new();
    let history = ws.root().join(".rustman/history");
    let grant = format!("net:localhost:{}", server.port());
    let manifest = fixtures::manifest("auth-fetch", &["generator"], &["net"]);
    let verified = verified(
        "auth-fetch",
        fixtures::fetcher(),
        &manifest,
        &[grant.as_str()],
    );
    // The policy allows another origin only.
    let plugin = plugin(
        &verified,
        context(ws.root(), "https://api.example.com/", &history),
    );

    let (_, result) = generate(plugin, server.url("/echo")).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "plugin auth-fetch: host not allowed"
    );
    assert!(history_texts(&history).is_empty());
}
