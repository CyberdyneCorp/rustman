//! Chain pinning, client detection and the chain registry against recorded
//! node answers (task 1.3), and through the real JSON-RPC transport.

use std::path::PathBuf;
use std::sync::Arc;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_evm::quantity::Form;
use rustman_evm::{
    ChainRegistry, ClientKind, Denomination, EvmSettings, Identities, JsonrpcTransport, Pin,
    Quantity, Recorded,
};
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, RpcReply};
use serde_json::{Value, json};

fn node(name: &str) -> Recorded {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/nodes/{name}.json"));
    Recorded::from_file(&format!("http://{name}.test/"), &path).unwrap()
}

#[tokio::test]
async fn geth_is_detected_with_its_version() {
    let rpc = node("geth");
    let identity = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), None)
        .await
        .unwrap();
    assert_eq!(identity.client.to_string(), "geth 1.14.11");
    assert_eq!(identity.status(), "Ethereum Mainnet (1) · geth 1.14.11");
    assert_eq!(
        identity.to_json(),
        json!({"chain_id": 1, "chain": "Ethereum Mainnet", "client": "geth", "client_version": "1.14.11", "network": null})
    );
    assert_eq!(rpc.methods(), ["eth_chainId", "web3_clientVersion"]);
}

#[tokio::test]
async fn reth_and_anvil_are_detected() {
    let registry = ChainRegistry::built_in();
    let reth = Identities::new()
        .identify(&node("reth"), &registry, None)
        .await
        .unwrap();
    assert_eq!(
        (reth.client.kind, reth.chain.label()),
        (ClientKind::Reth, "Base (8453)".into())
    );
    let anvil = Identities::new()
        .identify(&node("anvil"), &registry, None)
        .await
        .unwrap();
    assert_eq!(anvil.status(), "Anvil/Hardhat local (31337) · anvil 0.2.0");
}

#[tokio::test]
async fn an_unknown_client_is_not_an_error_and_net_version_names_the_network() {
    let rpc = node("unknown");
    let identity = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), None)
        .await
        .unwrap();
    assert_eq!(identity.client.to_string(), "unknown");
    assert_eq!(identity.network.as_deref(), Some("999"));
    assert_eq!(identity.chain.label(), "chain 999");
}

#[tokio::test]
async fn a_mismatch_stops_after_eth_chain_id() {
    let rpc = node("geth");
    let pin = Pin {
        environment: "sepolia",
        chain_id: 11_155_111,
    };
    let err = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), Some(pin))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)"
    );
    assert_eq!(err.exit_code(), 4);
    assert_eq!(rpc.methods(), ["eth_chainId"]);
}

#[tokio::test]
async fn identities_are_cached_per_endpoint_and_pins_still_apply() {
    let rpc = node("reth");
    let identities = Identities::new();
    let registry = ChainRegistry::built_in();
    identities.identify(&rpc, &registry, None).await.unwrap();
    let again = identities.identify(&rpc, &registry, None).await.unwrap();
    assert_eq!(again.chain.label(), "Base (8453)");
    assert_eq!(rpc.methods(), ["eth_chainId", "web3_clientVersion"]);
    let pin = Pin {
        environment: "mainnet",
        chain_id: 1,
    };
    let err = identities
        .identify(&rpc, &registry, Some(pin))
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("chain mismatch"), "{err}");
}

#[tokio::test]
async fn a_custom_chain_formats_native_values_in_its_symbol() {
    let table: toml::Table =
        toml::from_str("[chains.999]\nname = \"My L2\"\nsymbol = \"MYL\"\n").unwrap();
    let registry =
        ChainRegistry::with_overrides(&EvmSettings::from_table(&table).unwrap()).unwrap();
    let identity = Identities::new()
        .identify(&node("unknown"), &registry, None)
        .await
        .unwrap();
    assert_eq!(identity.chain.label(), "My L2 (999)");
    let balance = Quantity::from_rpc("0x0de0b6b3a7640000").unwrap();
    let native = Denomination::native(&identity.chain);
    assert_eq!(balance.show(Form::Native, &native), "1 MYL");
}

fn endpoint(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "POST".to_owned(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

fn reply(request: &Value) -> RpcReply {
    let id = request["id"].clone();
    let body = match request["method"].as_str() {
        Some("eth_chainId") => json!({"jsonrpc": "2.0", "id": id, "result": "0x1"}),
        _ => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
        }
    };
    RpcReply::json(&body)
}

#[tokio::test]
async fn through_the_jsonrpc_client_a_mismatch_sends_only_eth_chain_id() {
    let server = JsonrpcHttpServer::start(|req, _| reply(&req.json())).await;
    let engine = HttpEngine::new();
    let rpc = JsonrpcTransport::new(
        endpoint(&server.url("/")),
        Arc::new(engine),
        SendOptions::default(),
    );
    let pin = Pin {
        environment: "sepolia",
        chain_id: 11_155_111,
    };
    let err = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), Some(pin))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 4);
    let methods: Vec<Value> = server
        .hits()
        .iter()
        .map(|h| h.json()["method"].clone())
        .collect();
    assert_eq!(methods, [json!("eth_chainId")]);
    let identity = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), None)
        .await
        .unwrap();
    assert_eq!(identity.client.to_string(), "unknown");
}

#[tokio::test]
async fn transport_errors_exit_with_3() {
    let server = JsonrpcHttpServer::start(|_, _| {
        RpcReply::raw(502, "text/html", "<html>bad gateway</html>")
    })
    .await;
    let engine = HttpEngine::new();
    let rpc = JsonrpcTransport::new(
        endpoint(&server.url("/")),
        Arc::new(engine),
        SendOptions::default(),
    );
    let err = Identities::new()
        .identify(&rpc, &ChainRegistry::built_in(), None)
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 3, "{err}");
}
