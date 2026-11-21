//! `evm-call` execution against stub nodes and recorded mainnet answers
//! (task 3.1): ENS arguments, token units, state overrides, reverts,
//! gas estimates and the unsupported-overrides error. The Anvil run is in
//! tests/anvil.rs.

mod common;

use std::collections::BTreeMap;

use rustman_core::model::{Scalar, StateOverride};
use rustman_evm::abi::{built_in, resolve_function};
use rustman_evm::call::{CallInputs, OVERRIDES_UNSUPPORTED, prepare, run_call};
use rustman_evm::ens::Ens;
use rustman_evm::tx::TokenCache;
use rustman_evm::{Denomination, Recorded};
use rustman_test_support::jsonrpc::JsonrpcHttpServer;
use serde_json::json;

use common::{error, result, transport};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

fn balance_inputs(holder: &str) -> CallInputs {
    CallInputs {
        to: USDC.to_owned(),
        args: vec![json!(holder)],
        ..CallInputs::default()
    }
}

#[tokio::test]
async fn an_erc20_balance_of_an_ens_name_is_shown_in_token_units() {
    // Scenario "ERC-20 balance call".
    let rpc = Recorded::from_file(
        "https://mainnet.test/",
        &common::fixture("mainnet/ens.json"),
    )
    .unwrap();
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let ens = Ens::new(&rpc);
    let request = prepare(
        &balance_inputs("vitalik.eth"),
        function,
        &Denomination::default(),
        Some(&ens),
    )
    .await
    .unwrap();
    let report = run_call(&rpc, &request, function, Some(&erc20), false)
        .await
        .unwrap()
        .with_token(&rpc, 1, request.to, &TokenCache::new())
        .await;
    assert_eq!(report.render(), "balance = 250.00 USDC (250000000)");
    assert_eq!(report.to_json()["outputs"], json!({"balance": "250000000"}));
    assert_eq!(
        report.to_json()["token"],
        json!({"decimals": 6, "symbol": "USDC"})
    );
}

#[tokio::test]
async fn an_ens_argument_without_a_mainnet_endpoint_fails() {
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let err = prepare(
        &balance_inputs("vitalik.eth"),
        function,
        &Denomination::default(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "ENS needs a mainnet RPC; set [evm] ens_rpc"
    );
}

fn overridden() -> CallInputs {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "0x00000000000000000000000000000000000000ab".to_owned(),
        StateOverride {
            balance: Some(Scalar("100 ether".into())),
            ..StateOverride::default()
        },
    );
    CallInputs {
        block: Some("19000000".into()),
        state_overrides: Some(overrides),
        ..balance_inputs("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
    }
}

#[tokio::test]
async fn overrides_are_the_third_param_and_a_node_without_them_is_reported() {
    // Scenarios "Balance override" and "Historical call"; then a node that
    // rejects a third eth_call parameter.
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        error(&body, -32602, "too many arguments, want at most 2")
    })
    .await;
    let rpc = transport(&server.url("/"));
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let request = prepare(&overridden(), function, &Denomination::default(), None)
        .await
        .unwrap();
    let err = run_call(&rpc, &request, function, Some(&erc20), false)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), OVERRIDES_UNSUPPORTED);
    assert_eq!(err.to_string(), "node does not support state overrides");
    let sent = server.hits()[0].json();
    assert_eq!(sent["method"], "eth_call");
    assert_eq!(sent["params"][1], "0x121eac0", "block 19000000");
    assert_eq!(
        sent["params"][2],
        json!({"0x00000000000000000000000000000000000000AB": {"balance": "0x56bc75e2d63100000"}})
    );
}

#[tokio::test]
async fn a_revert_is_decoded_and_the_estimate_reported() {
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        match body["method"].as_str() {
            // Panic(0x12)
            Some("eth_call") => common::revert(
                &body,
                "0x4e487b710000000000000000000000000000000000000000000000000000000000000012",
            ),
            Some("eth_estimateGas") => result(&body, json!("0x5208")),
            _ => error(&body, -32601, "Method not found"),
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let inputs = balance_inputs("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
    let request = prepare(&inputs, function, &Denomination::default(), None)
        .await
        .unwrap();
    let report = run_call(&rpc, &request, function, Some(&erc20), true)
        .await
        .unwrap();
    assert_eq!(
        report.render(),
        "reverted: Panic(0x12) division or modulo by zero\ngas estimate: 21000"
    );
    assert_eq!(report.revert().and_then(|r| r.name()), Some("Panic"));
    assert_eq!(report.to_json()["gas_estimate"], "21000");
}

#[tokio::test]
async fn transport_errors_are_rpc_errors() {
    let server =
        JsonrpcHttpServer::start(|req, _| error(&req.json(), -32000, "header not found")).await;
    let rpc = transport(&server.url("/"));
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "totalSupply").unwrap();
    let inputs = CallInputs {
        to: USDC.to_owned(),
        ..CallInputs::default()
    };
    let request = prepare(&inputs, function, &Denomination::default(), None)
        .await
        .unwrap();
    let err = run_call(&rpc, &request, function, None, false)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "eth_call: rpc error -32000: header not found"
    );
    assert_eq!(err.exit_code(), 3);
}

#[tokio::test]
async fn a_secret_in_the_endpoint_never_reaches_call_trace_or_stream_output() {
    // Canary: the node echoes the API key of its URL in its errors.
    const KEY: &str = "evm-canary-k3y-0042";
    let server = JsonrpcHttpServer::start(|req, _| {
        error(&req.json(), -32000, &format!("invalid key {KEY}"))
    })
    .await;
    let mut endpoint = common::endpoint(&server.url(&format!("/v2/{KEY}")));
    endpoint.secrets = vec![rustman_core::Secret::new("rpc_key", KEY.to_owned())];
    let rpc = rustman_evm::JsonrpcTransport::new(
        endpoint,
        std::sync::Arc::new(rustman_http::HttpEngine::new()),
        rustman_http::SendOptions::default(),
    );
    assert!(!rustman_evm::Rpc::endpoint(&rpc).contains(KEY));
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "totalSupply").unwrap();
    let inputs = CallInputs {
        to: USDC.to_owned(),
        ..CallInputs::default()
    };
    let request = prepare(&inputs, function, &Denomination::default(), None)
        .await
        .unwrap();
    let call = run_call(&rpc, &request, function, None, true)
        .await
        .unwrap_err()
        .to_string();
    let trace = rustman_evm::trace::trace_call(
        &rpc,
        &request,
        &rustman_evm::NodeClient::unknown(),
        &rustman_evm::trace::TraceSupport::new(),
    )
    .await;
    let (tx, _rx) = rustman_evm::stream::item_channel();
    let stream = rustman_evm::stream::stream(
        &rpc,
        &rustman_evm::stream::StreamKind::Heads,
        &rustman_evm::stream::StreamConfig::default(),
        &tx,
    )
    .await
    .unwrap_err()
    .to_string();
    for shown in [call, format!("{trace:?}"), stream] {
        assert!(!shown.contains(KEY), "{shown}");
        assert!(shown.contains("invalid key"), "{shown}");
    }
}
