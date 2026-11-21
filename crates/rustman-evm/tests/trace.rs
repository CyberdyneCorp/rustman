//! Call traces against stub nodes (task 3.2): degrade when neither trace
//! method exists, fall back to `trace_call`, and remember per endpoint.
//! The `debug_traceCall` run against Anvil is in tests/anvil.rs.

mod common;

use std::sync::Arc;

use rustman_evm::abi::{built_in, resolve_function};
use rustman_evm::call::{CallInputs, prepare, run_call};
use rustman_evm::trace::{TraceResult, TraceSupport, TraceView, trace_call};
use rustman_evm::{Denomination, NodeClient};
use rustman_test_support::jsonrpc::JsonrpcHttpServer;
use serde_json::json;

use common::{error, result, transport};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const HOLDER: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

async fn balance_request() -> rustman_evm::call::CallRequest {
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let inputs = CallInputs {
        to: USDC.to_owned(),
        args: vec![json!(HOLDER)],
        ..CallInputs::default()
    };
    prepare(&inputs, function, &Denomination::default(), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn without_trace_methods_the_call_result_stands_with_a_note() {
    // Scenario "Degrade without tracing".
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        match body["method"].as_str() {
            Some("eth_call") => result(&body, json!(format!("0x{:064x}", 5))),
            _ => error(&body, -32601, "the method does not exist/is not available"),
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let erc20 = built_in("erc20").unwrap();
    let function = resolve_function(&erc20, "balanceOf").unwrap();
    let request = balance_request().await;
    let report = run_call(&rpc, &request, function, Some(&erc20), false)
        .await
        .unwrap();
    let geth = NodeClient::parse("Geth/v1.14.11-stable/linux-amd64/go1.23.2");
    let support = TraceSupport::new();
    let trace = trace_call(&rpc, &request, &geth, &support).await;
    assert_eq!(
        trace,
        TraceResult::Unsupported("tracing not supported by geth".into())
    );
    assert_eq!(report.render(), "balance = 5");
    let methods: Vec<String> = server
        .hits()
        .iter()
        .map(|h| h.json()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(methods, ["eth_call", "debug_traceCall", "trace_call"]);
    // Remembered for the endpoint: no second attempt.
    let again = trace_call(&rpc, &request, &geth, &support).await;
    assert!(matches!(again, TraceResult::Unsupported(_)));
    assert_eq!(server.hits().len(), 3);
}

#[tokio::test]
async fn trace_call_is_used_when_debug_trace_call_is_missing() {
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        match body["method"].as_str() {
            Some("trace_call") => result(
                &body,
                json!({"output": "0x", "trace": [
                    {"action": {"callType": "call", "from": HOLDER, "to": USDC, "gas": "0x1000",
                                "input": "0x18160ddd", "value": "0x0"},
                     "result": {"gasUsed": "0x100", "output": "0x"}, "traceAddress": [], "type": "call"},
                    {"action": {"callType": "delegatecall", "from": USDC, "to": HOLDER, "gas": "0x800",
                                "input": "0x18160ddd"},
                     "result": {"gasUsed": "0x80", "output": "0x"}, "traceAddress": [0], "type": "call"}
                ]}),
            ),
            _ => error(&body, -32601, "Method not found"),
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let request = balance_request().await;
    let support = TraceSupport::new();
    let TraceResult::Tree(root) =
        trace_call(&rpc, &request, &NodeClient::unknown(), &support).await
    else {
        panic!("no tree");
    };
    let erc20 = built_in("erc20").unwrap();
    let abis = move |_| vec![Arc::clone(&erc20)];
    let view = TraceView { abis: &abis };
    assert_eq!(
        view.render(&root),
        format!(
            "CALL {HOLDER} -> {USDC} totalSupply() gas=4096 used=256\n  DELEGATECALL {USDC} -> {HOLDER} totalSupply() gas=2048 used=128"
        )
    );
    let methods = server.hits().len();
    trace_call(&rpc, &request, &NodeClient::unknown(), &support).await;
    assert_eq!(server.hits().len(), methods + 1, "trace_call directly");
}

#[tokio::test]
async fn a_trace_error_is_not_a_call_error() {
    let server = JsonrpcHttpServer::start(|req, _| error(&req.json(), -32000, "out of gas")).await;
    let rpc = transport(&server.url("/"));
    let request = balance_request().await;
    let trace = trace_call(&rpc, &request, &NodeClient::unknown(), &TraceSupport::new()).await;
    assert_eq!(
        trace,
        TraceResult::Failed("debug_traceCall: rpc error -32000: out of gas".into())
    );
}
