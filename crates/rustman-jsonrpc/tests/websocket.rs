//! Calls, batches, the per-run connection pool and subscriptions over
//! WebSocket against a local stub (add-jsonrpc-client tasks 3.1 and 3.2).

mod common;

use std::time::{Duration, Instant};

use common::{call, endpoint, error, idempotent, result, target, timeout_ms};
use rustman_core::model::SubscriptionSpec;
use rustman_core::vars::{Resolved, ResolvedJsonrpc, ResolvedPair};
use rustman_http::{HttpEngine, SendOptions};
use rustman_jsonrpc::{
    CallOutcome, JsonrpcClient, RpcExchange, SubscribeOptions, SubscriptionEnd, SubscriptionLimits,
    WsPool,
};
use rustman_test_support::{JsonrpcWsServer, WsFrame};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn send(target: &ResolvedJsonrpc, options: &SendOptions) -> RpcExchange {
    let engine = HttpEngine::new();
    JsonrpcClient::new(&engine).send(target, options).await
}

/// The subscription id the stub hands out.
const SUB: &str = "0xsub";

fn head(n: u64, subscription: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "eth_subscription",
        "params": {"subscription": subscription, "result": {"number": format!("0x{n:x}")}},
    })
}

/// A node: `eth_blockNumber` answers after an unrelated notification,
/// `eth_subscribe` answers then emits `events` heads (one for another
/// subscription first), `pause` apart; `eth_unsubscribe` answers `true`.
fn node(events: u64, pause: Duration) -> impl Fn(&Value) -> Vec<WsFrame> + Send + Sync + 'static {
    move |message| {
        let id = message["id"].clone();
        let frames = match message["method"].as_str().unwrap_or_default() {
            "eth_blockNumber" => vec![head(0, "0xother"), result(id, json!("0x10"))],
            "eth_subscribe" => {
                let mut frames = vec![result(id, json!(SUB)), head(99, "0xother")];
                frames.extend((1..=events).map(|n| head(n, SUB)));
                return with_pauses(frames, pause);
            }
            "eth_unsubscribe" => vec![result(id, json!(true))],
            _ => vec![error(id, -32601, "Method not found")],
        };
        frames.into_iter().map(WsFrame::Json).collect()
    }
}

fn with_pauses(values: Vec<Value>, pause: Duration) -> Vec<WsFrame> {
    values
        .into_iter()
        .flat_map(|v| [WsFrame::Pause(pause), WsFrame::Json(v)])
        .collect()
}

fn new_heads(max_events: Option<u32>, max_duration_ms: Option<u64>) -> SubscriptionSpec {
    SubscriptionSpec {
        subscribe: "eth_subscribe".into(),
        unsubscribe: "eth_unsubscribe".into(),
        notification: "eth_subscription".into(),
        notification_id_path: None,
        max_events,
        max_duration_ms,
    }
}

fn subscription(url: &str, spec: SubscriptionSpec) -> ResolvedJsonrpc {
    let params = Some(json!(["newHeads"]));
    let calls = vec![rustman_core::vars::ResolvedCall::new(
        "eth_subscribe",
        params,
    )];
    let mut target = target(url, calls, false);
    target.subscription = Some(spec);
    target
}

#[tokio::test]
async fn an_unrelated_frame_before_the_response_is_ignored() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let exchange = send(
        &target(&server.url("/"), vec![call("eth_blockNumber")], false),
        &SendOptions::default(),
    )
    .await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    assert_eq!(
        exchange.calls[0].outcome,
        CallOutcome::Result(json!("0x10"))
    );
    assert_eq!(
        exchange.sent,
        r#"{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}"#
    );
    let sessions = server.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].methods(), ["eth_blockNumber"]);
}

#[tokio::test]
async fn the_handshake_carries_the_request_headers() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let mut request = target(&server.url("/"), vec![call("eth_blockNumber")], false);
    request.http.headers.push(ResolvedPair {
        name: Resolved::literal("X-Api-Key"),
        value: Resolved::literal("k-1"),
    });
    let exchange = send(&request, &SendOptions::default()).await;
    assert!(exchange.passed());
    assert_eq!(server.sessions()[0].header("x-api-key"), Some("k-1"));
}

#[tokio::test]
async fn a_batch_goes_out_as_one_frame() {
    let server = JsonrpcWsServer::start(|message| {
        let answers: Vec<Value> = message
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .map(|c| result(c["id"].clone(), c["method"].clone()))
            .collect();
        vec![
            WsFrame::Json(head(1, "0xother")),
            WsFrame::Json(Value::Array(answers)),
        ]
    })
    .await;
    let batch = target(
        &server.url("/"),
        vec![call("eth_chainId"), call("eth_blockNumber")],
        true,
    );
    let exchange = send(&batch, &SendOptions::default()).await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    assert_eq!(
        exchange.calls[1].outcome,
        CallOutcome::Result(json!("eth_blockNumber"))
    );
}

#[tokio::test]
async fn a_silent_server_times_out_per_call() {
    let server = JsonrpcWsServer::start(|_| Vec::new()).await;
    let exchange = send(
        &target(&server.url("/"), vec![call("m")], false),
        &timeout_ms(300),
    )
    .await;
    assert_eq!(
        exchange.error.unwrap().to_string(),
        "timeout after 300 ms (response)"
    );
}

#[tokio::test]
async fn a_run_pool_reuses_one_connection_per_endpoint_and_headers() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let engine = HttpEngine::new();
    let pool = WsPool::default();
    let client = JsonrpcClient::new(&engine).with_pool(&pool);
    let request = target(&server.url("/"), vec![call("eth_blockNumber")], false);
    for _ in 0..2 {
        let exchange = client.send(&request, &SendOptions::default()).await;
        assert!(exchange.passed(), "{:?}", exchange.error);
    }
    assert_eq!(server.sessions().len(), 1);
    let ids: Vec<Value> = server.sessions()[0]
        .messages
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    assert_eq!(
        ids,
        [json!(1), json!(2)],
        "ids are sequential per connection"
    );
    let mut other = request.clone();
    other.http.headers.push(ResolvedPair {
        name: Resolved::literal("X-Tenant"),
        value: Resolved::literal("b"),
    });
    assert!(client.send(&other, &SendOptions::default()).await.passed());
    assert_eq!(
        server.sessions().len(),
        2,
        "other headers, other connection"
    );
    assert_eq!(pool.len().await, 2);
    pool.close_all().await;
    assert!(pool.is_empty().await);
}

#[tokio::test]
async fn without_a_pool_each_exchange_has_its_own_connection() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let request = target(&server.url("/"), vec![call("eth_blockNumber")], false);
    for _ in 0..2 {
        assert!(send(&request, &SendOptions::default()).await.passed());
    }
    assert_eq!(server.sessions().len(), 2);
}

#[tokio::test]
async fn new_heads_stop_at_max_events_and_unsubscribe() {
    let server = JsonrpcWsServer::start(node(5, Duration::from_millis(5))).await;
    let exchange = send(
        &subscription(&server.url("/"), new_heads(Some(3), None)),
        &SendOptions::default(),
    )
    .await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    let record = exchange
        .subscription
        .as_ref()
        .expect("a subscription record");
    assert_eq!(record.subscription_id, json!(SUB));
    assert_eq!(record.events.len(), 3);
    assert_eq!(record.event_count, 3);
    assert_eq!(record.ended, SubscriptionEnd::MaxEvents);
    assert!(record.unsubscribed);
    assert!(
        record
            .events
            .iter()
            .all(|e| e["params"]["subscription"] == SUB)
    );
    assert_eq!(exchange.envelope(0)["events"].as_array().unwrap().len(), 3);
    assert_eq!(exchange.envelope(0)["result"], SUB);
    let sent = &server.sessions()[0].messages;
    assert_eq!(sent[0]["params"], json!(["newHeads"]));
    let unsubscribe = sent
        .iter()
        .find(|m| m["method"] == "eth_unsubscribe")
        .expect("eth_unsubscribe sent");
    assert_eq!(unsubscribe["params"], json!([SUB]));
}

#[tokio::test]
async fn the_duration_bound_ends_a_quiet_subscription() {
    let server = JsonrpcWsServer::start(node(1, Duration::ZERO)).await;
    let started = Instant::now();
    let spec = new_heads(Some(10), Some(300));
    let exchange = send(
        &subscription(&server.url("/"), spec),
        &SendOptions::default(),
    )
    .await;
    let record = exchange.subscription.expect("a subscription record");
    assert_eq!(record.ended, SubscriptionEnd::Duration);
    assert_eq!(record.events.len(), 1);
    assert!(record.unsubscribed);
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(
        server.sessions()[0]
            .methods()
            .contains(&"eth_unsubscribe".to_owned())
    );
}

#[tokio::test]
async fn an_interactive_subscription_runs_until_stopped() {
    let server = JsonrpcWsServer::start(node(1, Duration::ZERO)).await;
    let cancel = CancellationToken::new();
    let (feed, mut live) = tokio::sync::mpsc::unbounded_channel();
    let options = SubscribeOptions {
        limits: Some(SubscriptionLimits::interactive()),
        cancel: cancel.clone(),
        events: Some(feed),
        ..SubscribeOptions::default()
    };
    let engine = HttpEngine::new();
    let client = JsonrpcClient::new(&engine).with_subscribe(options);
    let request = subscription(&server.url("/"), new_heads(Some(1), Some(10)));
    let stopper = tokio::spawn(async move {
        let first = live.recv().await;
        cancel.cancel();
        first
    });
    let exchange = client.send(&request, &SendOptions::default()).await;
    let first = stopper.await.unwrap().expect("a live event");
    assert_eq!(first["params"]["subscription"], SUB);
    let record = exchange.subscription.expect("a subscription record");
    assert_eq!(record.ended, SubscriptionEnd::Stopped);
    assert!(record.unsubscribed);
}

#[tokio::test]
async fn the_notification_id_path_can_be_overridden() {
    let server = JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("accountSubscribe") => vec![
                WsFrame::Json(result(id, json!(7))),
                WsFrame::Json(json!({"jsonrpc":"2.0","method":"accountNotification","params":{"sub":7,"result":{"lamports":1}}})),
            ],
            _ => vec![WsFrame::Json(result(id, json!(true)))],
        }
    })
    .await;
    let spec = SubscriptionSpec {
        subscribe: "accountSubscribe".into(),
        unsubscribe: "accountUnsubscribe".into(),
        notification: "accountNotification".into(),
        notification_id_path: Some("$.params.sub".into()),
        max_events: Some(1),
        max_duration_ms: None,
    };
    let exchange = send(
        &subscription(&server.url("/"), spec),
        &SendOptions::default(),
    )
    .await;
    let record = exchange.subscription.expect("a subscription record");
    assert_eq!(record.events.len(), 1);
    assert_eq!(record.events[0]["params"]["result"]["lamports"], 1);
}

#[tokio::test]
async fn a_result_with_a_null_id_does_not_answer_the_subscribe_call() {
    // Regression: any null-id response was taken as the subscribe answer.
    let server = JsonrpcWsServer::start(|message| {
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("eth_subscribe") => vec![
                WsFrame::Json(result(Value::Null, json!("0xbogus"))),
                WsFrame::Json(result(id, json!(SUB))),
                WsFrame::Json(head(1, SUB)),
            ],
            _ => vec![WsFrame::Json(result(id, json!(true)))],
        }
    })
    .await;
    let exchange = send(
        &subscription(&server.url("/"), new_heads(Some(1), None)),
        &SendOptions::default(),
    )
    .await;
    let record = exchange.subscription.expect("a subscription record");
    assert_eq!(record.subscription_id, json!(SUB));
    assert_eq!(record.events.len(), 1);
}

#[tokio::test]
async fn a_null_id_error_answers_the_subscribe_call() {
    let server =
        JsonrpcWsServer::start(|_| vec![WsFrame::Json(error(Value::Null, -32700, "parse error"))])
            .await;
    let exchange = send(
        &subscription(&server.url("/"), new_heads(Some(1), None)),
        &timeout_ms(5_000),
    )
    .await;
    assert!(exchange.subscription.is_none());
    assert_eq!(exchange.failures(), ["rpc error -32700: parse error"]);
}

#[tokio::test]
async fn a_refused_subscribe_is_a_failed_call_without_events() {
    let server = JsonrpcWsServer::start(|message| {
        vec![WsFrame::Json(error(
            message["id"].clone(),
            -32602,
            "invalid params",
        ))]
    })
    .await;
    let exchange = send(
        &subscription(&server.url("/"), new_heads(None, None)),
        &SendOptions::default(),
    )
    .await;
    assert!(exchange.subscription.is_none());
    assert_eq!(exchange.failures(), ["rpc error -32602: invalid params"]);
}

#[tokio::test]
async fn a_refused_handshake_is_an_exchange_error() {
    let request = endpoint(&format!("ws://127.0.0.1:{}/", common::closed_port()));
    let target = ResolvedJsonrpc::new(request, vec![call("m")], false);
    let exchange = send(&target, &SendOptions::default()).await;
    let err = exchange.error.expect("no connection");
    assert!(err.to_string().contains("cannot connect"), "{err}");
}

#[tokio::test]
async fn an_http_proxy_tunnels_the_websocket_with_connect() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let proxy = rustman_test_support::TestProxy::http().await;
    let options = SendOptions {
        proxy: rustman_http::ProxySettings::explicit(&proxy.url()).unwrap(),
        ..SendOptions::default()
    };
    let exchange = send(
        &target(&server.url("/"), vec![call("eth_blockNumber")], false),
        &options,
    )
    .await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    let log = proxy.log();
    let authority = server.url("").trim_start_matches("ws://").to_owned();
    assert!(
        log.iter()
            .any(|l| l.starts_with(&format!("CONNECT {authority} HTTP/1.1"))),
        "{log:?}"
    );
}

/// A message over `max_body_bytes` is refused, and never retried.
#[tokio::test]
async fn a_message_over_max_body_bytes_is_refused() {
    let server = JsonrpcWsServer::start(|message: &Value| {
        vec![WsFrame::Json(result(
            message["id"].clone(),
            json!("y".repeat(64 * 1024)),
        ))]
    })
    .await;
    let read = idempotent(target(&server.url("/"), vec![call("big")], false), 2);
    let mut options = SendOptions::default();
    options.body.max_in_memory = 4096;
    let exchange = send(&read, &options).await;
    assert_eq!(
        exchange.error.as_ref().map(ToString::to_string).as_deref(),
        Some("WebSocket: a message is larger than the 4096 byte limit (max_body_bytes)")
    );
    assert_eq!(exchange.attempts.len(), 1);
    assert!(exchange.calls.is_empty());
}

/// A traced call over WebSocket carries `traceparent` on the handshake
/// (add-trace-propagation task 2.3).
#[tokio::test]
async fn a_traced_handshake_carries_a_traceparent() {
    let server = JsonrpcWsServer::start(node(0, Duration::ZERO)).await;
    let options = SendOptions {
        trace: Some(rustman_core::trace::TracePropagation::default()),
        ..SendOptions::default()
    };
    let exchange = send(
        &target(&server.url("/"), vec![call("eth_blockNumber")], false),
        &options,
    )
    .await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    let traceparent = server.sessions()[0]
        .header("traceparent")
        .map(str::to_owned);
    assert!(traceparent.is_some_and(|t| t.starts_with("00-") && t.len() == 55));
}
