//! Opt-in explorer ABI fetch (task 2.3): disabled by default with no
//! network call, EIP-1967 proxies resolved to their implementation, the
//! on-disk cache, and the API key kept out of every message.

use std::path::PathBuf;

use alloy_primitives::{Address, address};
use rustman_core::Secret;
use rustman_evm::abi::AbiCache;
use rustman_evm::explorer::{Explorer, FETCH_DISABLED, cache_path};
use rustman_evm::{EvmSettings, Recorded};
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, RpcReply};
use serde_json::json;

const PROXY: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");
const IMPLEMENTATION: Address = address!("e7f1725E7734CE288F8367e1Bb143E90bb3F0512");
const UNVERIFIED: Address = address!("9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");
const KEY: &str = "K3yCanary0123456789ABCDEF";

fn rpc() -> Recorded {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/explorer/proxy.json");
    Recorded::from_file("http://node.test/", &path).unwrap()
}

fn vault_abi() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/abi/Vault.abi.json");
    std::fs::read_to_string(path).unwrap()
}

/// An explorer answering the Vault ABI for the implementation only, and
/// an error echoing the request (key included) for anything else.
async fn explorer_server() -> JsonrpcHttpServer {
    let abi = vault_abi();
    JsonrpcHttpServer::start(move |req, _| {
        let implementation = format!("address={IMPLEMENTATION:#x}");
        if req.uri.contains(&implementation) {
            RpcReply::json(&json!({"status": "1", "message": "OK", "result": abi}))
        } else {
            let echo = format!("Contract source code not verified ({})", req.uri);
            RpcReply::json(&json!({"status": "0", "message": "NOTOK", "result": echo}))
        }
    })
    .await
}

fn settings(enabled: bool, api_url: &str) -> EvmSettings {
    let text = format!(
        "allow_explorer_fetch = {enabled}\n[explorers.31337]\napi_url = \"{api_url}\"\napi_key = \"{{{{explorer_key}}}}\"\n"
    );
    EvmSettings::from_table(&toml::from_str(&text).unwrap()).unwrap()
}

#[tokio::test]
async fn disabled_by_default_makes_no_network_call() {
    let server = explorer_server().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new();
    let settings = EvmSettings::default();
    let cache = AbiCache::new();
    let explorer = Explorer {
        settings: &settings,
        root: dir.path(),
        http: &engine,
        options: &SendOptions::default(),
        api_key: None,
        cache: &cache,
    };
    let node = rpc();
    let err = explorer.fetch(&node, 31_337, PROXY).await.unwrap_err();
    assert_eq!(err.to_string(), FETCH_DISABLED);
    assert_eq!(
        err.to_string(),
        "explorer ABI fetch is disabled; set [evm] allow_explorer_fetch = true"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(server.hits().is_empty());
    assert!(node.methods().is_empty());
    let configured = settings_with_server(&server, false);
    let explorer = Explorer {
        settings: &configured,
        ..explorer
    };
    assert!(explorer.fetch(&node, 31_337, PROXY).await.is_err());
    assert!(server.hits().is_empty());
}

fn settings_with_server(server: &JsonrpcHttpServer, enabled: bool) -> EvmSettings {
    settings(enabled, &server.url("/api"))
}

#[tokio::test]
async fn a_proxy_resolves_to_its_implementation_and_is_cached() {
    let server = explorer_server().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new();
    let settings = settings_with_server(&server, true);
    let cache = AbiCache::new();
    let explorer = Explorer {
        settings: &settings,
        root: dir.path(),
        http: &engine,
        options: &SendOptions::default(),
        api_key: Some(Secret::new("explorer_key", KEY.to_owned())),
        cache: &cache,
    };
    let node = rpc();
    let fetched = explorer.fetch(&node, 31_337, PROXY).await.unwrap();
    assert_eq!(
        fetched.note().as_deref(),
        Some("proxy → implementation 0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512")
    );
    assert!(!fetched.from_cache);
    assert!(fetched.loaded.abi.function("withdraw").is_some());
    let hits = server.hits();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].uri.contains("chainid=31337"), "{}", hits[0].uri);
    assert!(hits[0].uri.contains("action=getabi"));
    assert!(cache_path(dir.path(), 31_337, IMPLEMENTATION).is_file());
    let again = explorer.fetch(&node, 31_337, PROXY).await.unwrap();
    assert!(again.from_cache);
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test]
async fn explorer_errors_never_show_the_api_key() {
    let server = explorer_server().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new();
    let settings = settings_with_server(&server, true);
    let cache = AbiCache::new();
    let explorer = Explorer {
        settings: &settings,
        root: dir.path(),
        http: &engine,
        options: &SendOptions::default(),
        api_key: Some(Secret::new("explorer_key", KEY.to_owned())),
        cache: &cache,
    };
    let err = explorer
        .fetch(&rpc(), 31_337, UNVERIFIED)
        .await
        .unwrap_err();
    let shown = err.to_string();
    assert!(server.hits()[0].uri.contains(KEY));
    assert!(!shown.contains(KEY), "{shown}");
    assert!(shown.contains("«redacted:explorer_key»"), "{shown}");
    assert!(
        shown.starts_with("explorer: no verified ABI for 0x9fe4"),
        "{shown}"
    );
    let missing = explorer.fetch(&rpc(), 1, IMPLEMENTATION).await;
    assert!(missing.is_err());
}

#[tokio::test]
async fn a_chain_without_an_explorer_is_a_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new();
    let settings =
        EvmSettings::from_table(&toml::from_str("allow_explorer_fetch = true").unwrap()).unwrap();
    let cache = AbiCache::new();
    let explorer = Explorer {
        settings: &settings,
        root: dir.path(),
        http: &engine,
        options: &SendOptions::default(),
        api_key: None,
        cache: &cache,
    };
    let err = explorer
        .fetch(&rpc(), 31_337, IMPLEMENTATION)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "no explorer configured for chain 31337; add [evm.explorers.31337] api_url"
    );
}
