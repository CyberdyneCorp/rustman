//! Head and log streams against stub nodes (task 3.3): HTTP polling with
//! a forgotten filter and a reorged log, a WebSocket subscription, and the
//! decoded timeline line of a `removed: true` log. The Anvil WebSocket run
//! is in tests/anvil.rs.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use rustman_evm::stream::{LogFilter, StreamConfig, StreamItem, StreamKind, StreamMode, stream};
use rustman_evm::tx::{DecodeContext, TokenCache, log_line, log_view};
use rustman_evm::{ChainRegistry, Recorded};
use rustman_jsonrpc::SubscriptionEnd;
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, JsonrpcWsServer, WsFrame};
use serde_json::{Value, json};

use common::{error, result, transport};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const TRANSFER: &str = "Transfer(address,address,uint256)";

/// A USDC `Transfer` of 250 USDC, as a node sends it.
fn transfer_log(index: u64, removed: bool) -> Value {
    let word = |hex: &str| format!("0x{hex:0>64}");
    json!({
        "address": USDC,
        "topics": [
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
            word("d8da6bf26964af9d7eed9e03e53415d37aa96045"),
            word("00000000000000000000000000000000000000ab"),
        ],
        "data": word("ee6b280"),
        "logIndex": format!("{index:#x}"),
        "removed": removed,
    })
}

fn filter() -> StreamKind {
    StreamKind::Logs(LogFilter::parse(&[USDC.to_owned()], Some(TRANSFER), &[]).unwrap())
}

fn fast(max_events: u64) -> StreamConfig {
    StreamConfig {
        poll_interval: Duration::from_millis(20),
        max_events: Some(max_events),
        max_duration: Some(Duration::from_secs(20)),
        ..StreamConfig::default()
    }
}

fn drain(items: &mut tokio::sync::mpsc::Receiver<StreamItem>) -> Vec<StreamItem> {
    let mut all = Vec::new();
    while let Ok(item) = items.try_recv() {
        all.push(item);
    }
    all
}

#[tokio::test]
async fn http_polls_filter_changes_and_marks_reorgs() {
    // Scenarios "HTTP polling fallback" and "Reorg marked".
    // The node forgets the first filter: a new one is made.
    let server = JsonrpcHttpServer::start(|req, n| {
        let body = req.json();
        let polls = body["params"][0].as_str() == Some("0x2");
        match body["method"].as_str() {
            Some("eth_newFilter") if body["params"][0]["address"] == USDC => {
                result(&body, json!(if n == 0 { "0x1" } else { "0x2" }))
            }
            Some("eth_getFilterChanges") if !polls => error(&body, -32000, "filter not found"),
            Some("eth_getFilterChanges") => result(
                &body,
                json!([
                    transfer_log(1, false),
                    transfer_log(1, true),
                    transfer_log(2, false)
                ]),
            ),
            Some("eth_uninstallFilter") => result(&body, json!(true)),
            _ => error(&body, -32601, "Method not found"),
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let config = fast(2);
    assert_eq!(
        StreamMode::of(&server.url("/"), &StreamConfig::default())
            .note()
            .as_deref(),
        Some("polling every 2s (no WebSocket)")
    );
    let (tx, mut rx) = rustman_evm::stream::item_channel();
    let summary = stream(&rpc, &filter(), &config, &tx).await.unwrap();
    assert_eq!((summary.events, summary.reorged), (2, 1));
    assert_eq!(summary.ended, SubscriptionEnd::MaxEvents);
    let items = drain(&mut rx);
    assert_eq!(items.iter().filter(|i| i.removed()).count(), 1);
    let methods: Vec<String> = server
        .hits()
        .iter()
        .map(|h| h.json()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        methods,
        [
            "eth_newFilter",
            "eth_getFilterChanges",
            "eth_newFilter",
            "eth_getFilterChanges",
            "eth_uninstallFilter"
        ]
    );
    let topics = &server.hits()[0].json()["params"][0]["topics"];
    assert_eq!(
        topics[0],
        "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
    );
}

#[tokio::test]
async fn polling_stops_at_the_duration() {
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        match body["method"].as_str() {
            Some("eth_newBlockFilter") => result(&body, json!("0x9")),
            Some("eth_getFilterChanges") => result(&body, json!([])),
            _ => result(&body, json!(true)),
        }
    })
    .await;
    let config = StreamConfig {
        poll_interval: Duration::from_millis(20),
        max_duration: Some(Duration::from_millis(200)),
        ..StreamConfig::default()
    };
    let (tx, _rx) = rustman_evm::stream::item_channel();
    let summary = stream(
        &transport(&server.url("/")),
        &StreamKind::Heads,
        &config,
        &tx,
    )
    .await
    .unwrap();
    assert_eq!(summary.ended, SubscriptionEnd::Duration);
    assert_eq!(summary.events, 0);
}

#[tokio::test]
async fn websocket_endpoints_subscribe() {
    // Scenario "WebSocket subscription".
    let server = JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("eth_subscribe") => {
                let notify = |log: Value| {
                    WsFrame::Json(json!({"jsonrpc": "2.0", "method": "eth_subscription",
                        "params": {"subscription": "0xs1", "result": log}}))
                };
                vec![
                    WsFrame::Json(json!({"jsonrpc": "2.0", "id": id, "result": "0xs1"})),
                    WsFrame::Pause(Duration::from_millis(50)),
                    notify(transfer_log(1, false)),
                    notify(transfer_log(1, true)),
                    notify(transfer_log(2, false)),
                ]
            }
            _ => vec![WsFrame::Json(
                json!({"jsonrpc": "2.0", "id": id, "result": true}),
            )],
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let config = fast(2);
    assert_eq!(StreamMode::of(&server.url("/"), &config).note(), None);
    let (tx, mut rx) = rustman_evm::stream::item_channel();
    let summary = stream(&rpc, &filter(), &config, &tx).await.unwrap();
    assert_eq!((summary.events, summary.reorged), (2, 1));
    assert_eq!(summary.ended, SubscriptionEnd::MaxEvents);
    assert_eq!(drain(&mut rx).len(), 3);
    let sent = &server.sessions()[0].messages[0];
    assert_eq!(sent["method"], "eth_subscribe");
    assert_eq!(sent["params"][0], "logs");
    assert_eq!(sent["params"][1]["address"], USDC);
    assert_eq!(server.sessions()[0].methods()[1], "eth_unsubscribe");
}

#[tokio::test]
async fn a_removed_log_is_shown_as_reorged() {
    let recorded = Recorded::from_file(
        "https://mainnet.test/",
        &common::fixture("mainnet/ens.json"),
    )
    .unwrap();
    let abis = HashMap::new();
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc: &recorded,
        chain: ChainRegistry::built_in().get(1),
        abis: &abis,
        tokens: &tokens,
    };
    let mut warnings = Vec::new();
    let view = log_view(&ctx, &transfer_log(7, true), &mut warnings)
        .await
        .unwrap();
    assert_eq!(
        log_line(&view),
        format!(
            "REORGED [7] {USDC} Transfer(from=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, to=0x00000000000000000000000000000000000000AB, value=250.00 USDC)"
        )
    );
}

#[tokio::test]
async fn a_stream_waits_for_its_consumer_and_ends_when_it_is_gone() {
    // Regression: items went into an unbounded channel, so a node pushing
    // faster than the printer grew memory without limit.
    let server = JsonrpcHttpServer::start(|req, _| {
        let body = req.json();
        match body["method"].as_str() {
            Some("eth_newFilter") => result(&body, json!("0x1")),
            Some("eth_getFilterChanges") => {
                let logs: Vec<Value> = (0..100).map(|i| transfer_log(i, false)).collect();
                result(&body, json!(logs))
            }
            Some("eth_uninstallFilter") => result(&body, json!(true)),
            _ => error(&body, -32601, "Method not found"),
        }
    })
    .await;
    let rpc = transport(&server.url("/"));
    let config = StreamConfig {
        poll_interval: Duration::from_millis(1),
        max_events: None,
        max_duration: Some(Duration::from_secs(20)),
        ..StreamConfig::default()
    };
    let (tx, rx) = rustman_evm::stream::item_channel();
    let consumer = tokio::spawn(async move {
        let mut rx = rx;
        // Take a few items slowly, then go away.
        for _ in 0..3 {
            rx.recv().await.unwrap();
            assert!(rx.len() <= rustman_evm::stream::STREAM_BUFFER);
        }
    });
    let summary = stream(&rpc, &filter(), &config, &tx).await.unwrap();
    consumer.await.unwrap();
    assert!(
        summary.events <= (rustman_evm::stream::STREAM_BUFFER + 3 + 1) as u64,
        "the stream stopped at the buffer: {summary:?}"
    );
    assert_eq!(summary.ended, SubscriptionEnd::Stopped);
}
