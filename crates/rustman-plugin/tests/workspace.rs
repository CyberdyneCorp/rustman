//! Declaring, pinning and fetching plugins (plugins spec, "Declaring
//! plugins in the workspace", "Integrity pinning and lockfile", "URL
//! sources"; tasks 2.2 and 2.3).

mod common;

use common::Ws;
use rustman_plugin::fetch::{FetchOptions, MAX_DOWNLOAD_BYTES, fetch};
use rustman_plugin::fixtures;
use rustman_plugin::lock::Lockfile;
use rustman_plugin::pin::sha256_hex;
use rustman_plugin::source::cache_path;
use rustman_plugin::{LoadError, LoadOptions, load, verify_pins};
use rustman_test_support::{FileServer, TestCerts};

fn plugin_bytes() -> Vec<u8> {
    let manifest = fixtures::manifest("hmac", &["generator"], &["clock"]);
    fixtures::with_manifest(&fixtures::adversarial(), &manifest)
}

fn offline() -> LoadOptions {
    LoadOptions::default()
}

fn https_options(certs: &TestCerts) -> LoadOptions {
    LoadOptions {
        fetch: FetchOptions {
            ca_bundle: Some(certs.ca_pem.clone().into_bytes()),
            // The test server is on localhost.
            allow_local: true,
            ..FetchOptions::default()
        },
        allow_fetch: true,
    }
}

#[tokio::test]
async fn undeclared_plugin_files() {
    let ws = Ws::new();
    let wasm = fixtures::adversarial();
    ws.write("plugins/hmac/plugin.wasm", &wasm);
    ws.write(
        "plugins/hmac/rustman-plugin.toml",
        fixtures::manifest("hmac", &["generator"], &[]),
    );
    let set = load(ws.root(), &offline()).await.unwrap();
    assert!(set.is_empty());
    // Declared, the same folder loads.
    ws.declare("hmac", "plugins/hmac", &sha256_hex(&wasm), &[]);
    let set = load(ws.root(), &offline()).await.unwrap();
    assert_eq!(set.plugins.len(), 1);
    assert_eq!(set.plugins[0].manifest.name, "hmac");
}

#[tokio::test]
async fn tampered_plugin() {
    let ws = Ws::new();
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    let mut tampered = wasm.clone();
    tampered.push(0);
    ws.write("plugins/hmac.wasm", &tampered);
    ws.declare("hmac", "plugins/hmac.wasm", &pin, &[]);

    let err = load(ws.root(), &offline()).await.unwrap_err();

    let expected = format!(
        "plugin hmac: sha256 mismatch (expected {pin}, got {})",
        sha256_hex(&tampered)
    );
    assert_eq!(err.to_string(), expected);
    assert_eq!(err.exit_code(), 2);
    assert_eq!(verify_pins(ws.root()).unwrap_err().to_string(), expected);
    ws.write("plugins/hmac.wasm", &wasm);
    verify_pins(ws.root()).unwrap();
}

#[tokio::test]
async fn newer_api_major_is_skipped_and_the_rest_loads() {
    let ws = Ws::new();
    let current = plugin_bytes();
    let newer_manifest =
        fixtures::manifest("next", &["generator"], &[]).replace("api = 1", "api = 2");
    let newer = fixtures::with_manifest(&fixtures::adversarial(), &newer_manifest);
    ws.write("plugins/hmac.wasm", &current);
    ws.write("plugins/next.wasm", &newer);
    ws.declare(
        "hmac",
        "plugins/hmac.wasm",
        &sha256_hex(&current),
        &["clock"],
    );
    ws.declare("next", "plugins/next.wasm", &sha256_hex(&newer), &[]);

    let set = load(ws.root(), &offline()).await.unwrap();

    assert_eq!(set.plugins.len(), 1);
    assert_eq!(set.plugins[0].name(), "hmac");
    assert_eq!(set.plugins[0].grants, [rustman_plugin::grant::Grant::Clock]);
    assert_eq!(
        set.skipped,
        [LoadError::UnsupportedApi {
            plugin: "next".into(),
            api: 2,
            supported: "1".into(),
        }]
    );
}

#[tokio::test]
async fn grants_are_checked_against_the_manifest_on_load() {
    let ws = Ws::new();
    let wasm = plugin_bytes();
    ws.write("plugins/hmac.wasm", &wasm);
    ws.declare(
        "hmac",
        "plugins/hmac.wasm",
        &sha256_hex(&wasm),
        &["env:HOME"],
    );
    let err = load(ws.root(), &offline()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "plugin hmac: grant 'env:HOME' not requested by the plugin"
    );
}

#[tokio::test]
async fn the_lockfile_records_the_loaded_set() {
    let ws = Ws::new();
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    ws.write("plugins/hmac.wasm", &wasm);
    ws.declare("hmac", "plugins/hmac.wasm", &pin, &[]);
    let set = load(ws.root(), &offline()).await.unwrap();
    let mut lock = Lockfile::read(ws.root()).unwrap();
    for plugin in &set.plugins {
        lock.upsert(plugin.locked());
    }
    lock.write(ws.root()).unwrap();
    let text = std::fs::read_to_string(ws.root().join("rustman.lock")).unwrap();
    assert!(text.contains("name = \"hmac\""), "{text}");
    assert!(text.contains("version = \"0.1.0\""), "{text}");
    assert!(text.contains("source = \"plugins/hmac.wasm\""), "{text}");
    assert!(text.contains(&format!("sha256 = \"{pin}\"")), "{text}");
    assert!(text.contains("api = 1"), "{text}");
}

#[tokio::test]
async fn url_sources_are_fetched_once_and_cached() {
    let certs = TestCerts::generate();
    let server = FileServer::https(&certs).await;
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    server.serve("/hmac-0.2.0.wasm", wasm.clone());
    let ws = Ws::new();
    ws.declare("hmac", &server.url("/hmac-0.2.0.wasm"), &pin, &[]);

    // Not cached and no download allowed: refused.
    let err = load(ws.root(), &offline()).await.unwrap_err();
    assert!(err.to_string().contains("is not downloaded yet"), "{err}");

    let set = load(ws.root(), &https_options(&certs)).await.unwrap();
    assert_eq!(set.plugins[0].manifest.name, "hmac");
    assert_eq!(server.hits("/hmac-0.2.0.wasm"), 1);
    assert_eq!(std::fs::read(cache_path(ws.root(), &pin)).unwrap(), wasm);
}

/// A committed URL source naming this machine (or a private network) is
/// not downloaded by `plugin verify` or a load: nothing is sent to it.
#[tokio::test]
async fn committed_local_sources_are_not_downloaded() {
    let certs = TestCerts::generate();
    let server = FileServer::https(&certs).await;
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    server.serve("/hmac.wasm", wasm);
    let ws = Ws::new();
    let url = server.url("/hmac.wasm");
    ws.declare("hmac", &url, &pin, &[]);
    let mut options = https_options(&certs);
    options.fetch.allow_local = false;
    let err = load(ws.root(), &options).await.unwrap_err().to_string();
    assert!(
        err.contains("is this machine or a private network"),
        "{err}"
    );
    assert_eq!(server.hits("/hmac.wasm"), 0);
    assert!(!cache_path(ws.root(), &pin).exists());
}

#[tokio::test]
async fn no_silent_upgrade() {
    let certs = TestCerts::generate();
    let server = FileServer::https(&certs).await;
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    server.serve("/hmac.wasm", wasm.clone());
    let ws = Ws::new();
    ws.declare("hmac", &server.url("/hmac.wasm"), &pin, &[]);
    load(ws.root(), &https_options(&certs)).await.unwrap();

    // The URL now serves a newer build.
    let newer = fixtures::with_manifest(
        &fixtures::adversarial(),
        &fixtures::manifest("hmac", &["generator"], &["random"]),
    );
    server.serve("/hmac.wasm", newer.clone());
    let set = load(ws.root(), &https_options(&certs)).await.unwrap();

    assert_eq!(server.hits("/hmac.wasm"), 1, "a second download happened");
    assert_eq!(&*set.plugins[0].wasm, wasm.as_slice());

    // Without the cache, the newer build is fetched and refused by the pin.
    std::fs::remove_file(cache_path(ws.root(), &pin)).unwrap();
    let err = load(ws.root(), &https_options(&certs)).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "plugin hmac: sha256 mismatch (expected {pin}, got {})",
            sha256_hex(&newer)
        )
    );
    assert!(!cache_path(ws.root(), &pin).exists());
}

#[tokio::test]
async fn a_tampered_cache_is_refused() {
    let ws = Ws::new();
    let wasm = plugin_bytes();
    let pin = sha256_hex(&wasm);
    ws.declare("hmac", "https://example.com/hmac.wasm", &pin, &[]);
    ws.write(&format!(".rustman/plugins/{pin}.wasm"), b"tampered");
    let err = load(ws.root(), &offline()).await.unwrap_err();
    assert!(matches!(err, LoadError::Mismatch { .. }), "{err}");
    assert!(verify_pins(ws.root()).is_err());
}

#[tokio::test]
async fn local_https_download() {
    let certs = TestCerts::generate();
    let server = FileServer::https(&certs).await;
    server.serve("/p.wasm", b"component bytes".to_vec());
    let options = https_options(&certs).fetch;
    let bytes = fetch("p", &server.url("/p.wasm"), &options).await.unwrap();
    assert_eq!(bytes, b"component bytes");
    // Redirects are followed while they stay on https.
    let redirect = server.url(&format!("/redirect?to={}", server.url("/p.wasm")));
    assert_eq!(
        fetch("p", &redirect, &options).await.unwrap(),
        b"component bytes"
    );
    // TLS verification is always on: without the test CA, it fails.
    let no_ca = FetchOptions {
        allow_local: true,
        ..FetchOptions::default()
    };
    let hits = server.hits("/p.wasm");
    let err = fetch("p", &server.url("/p.wasm"), &no_ca)
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with("plugin p: download of "),
        "{err}"
    );
    assert!(!err.to_string().contains("private network"), "{err}");
    assert_eq!(server.hits("/p.wasm"), hits);
}

#[tokio::test]
async fn plain_http_refused() {
    let plain = FileServer::http().await;
    plain.serve("/p.wasm", b"component bytes".to_vec());
    let err = fetch("p", &plain.url("/p.wasm"), &FetchOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "plugin sources must use https");
    assert_eq!(err.exit_code(), 2);
    assert_eq!(plain.hits("/p.wasm"), 0);
    // Nor through a redirect from https.
    let certs = TestCerts::generate();
    let secure = FileServer::https(&certs).await;
    let redirect = secure.url(&format!("/redirect?to={}", plain.url("/p.wasm")));
    let err = fetch("p", &redirect, &https_options(&certs).fetch)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "plugin sources must use https");
    assert_eq!(plain.hits("/p.wasm"), 0);
    // A declared http source never loads.
    let ws = Ws::new();
    ws.declare("p", &plain.url("/p.wasm"), &"a".repeat(64), &[]);
    let err = load(ws.root(), &https_options(&certs)).await.unwrap_err();
    assert_eq!(err, LoadError::PlainHttp);
}

#[tokio::test]
async fn downloads_are_capped_at_20_mib() {
    let certs = TestCerts::generate();
    let server = FileServer::https(&certs).await;
    let limit = usize::try_from(MAX_DOWNLOAD_BYTES).unwrap();
    server.serve("/big.wasm", vec![0u8; limit + 1]);
    server.serve("/max.wasm", vec![0u8; limit]);
    let options = https_options(&certs).fetch;
    let err = fetch("p", &server.url("/big.wasm"), &options)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("larger than 20971520 bytes"),
        "{err}"
    );
    assert_eq!(
        fetch("p", &server.url("/max.wasm"), &options)
            .await
            .unwrap()
            .len(),
        limit
    );
}
