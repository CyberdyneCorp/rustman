//! Calls, batches, notifications and retries over HTTP against a local
//! stub (add-jsonrpc-client tasks 2.1 to 2.4).

mod common;

use std::fs;
use std::time::Duration;

use common::{call, error, idempotent, result, target, timeout_ms};
use rustman_core::Workspace;
use rustman_core::vars::{NoSecrets, Resolver, resolve_jsonrpc};
use rustman_http::{HttpEngine, SendOptions};
use rustman_jsonrpc::{CallOutcome, JsonrpcClient, JsonrpcError, RpcExchange};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::{Value, json};

async fn send(target: &rustman_core::vars::ResolvedJsonrpc, options: &SendOptions) -> RpcExchange {
    let engine = HttpEngine::new();
    JsonrpcClient::new(&engine).send(target, options).await
}

/// A server answering every call of a body with `result: "0x10"`.
fn echo_result(body: &Value) -> Value {
    match body {
        Value::Array(calls) => calls
            .iter()
            .map(|c| result(c["id"].clone(), json!("0x10")))
            .collect(),
        single => result(single["id"].clone(), json!("0x10")),
    }
}

#[tokio::test]
async fn a_minimal_call_from_a_request_file_sends_the_exact_envelope() {
    let server = JsonrpcHttpServer::start(|req, _| RpcReply::json(&echo_result(&req.json()))).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("rustman.toml"),
        format!(
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nrpc_url = \"{}\"\n",
            server.url("/rpc")
        ),
    )
    .unwrap();
    fs::write(
        root.join("block.rman.yaml"),
        "version: 1\nkind: jsonrpc\nname: block\nendpoint: '{{rpc_url}}'\nmethod: eth_blockNumber\n",
    )
    .unwrap();
    let ws = Workspace::load(root).unwrap();
    let request = &ws.find_by_name("block")[0];
    let resolver = Resolver::for_request(&ws.settings, request, None, &NoSecrets);
    let resolved = resolve_jsonrpc(request, &resolver).unwrap();
    let exchange = send(&resolved, &SendOptions::default()).await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(
        hit.body_text(),
        r#"{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}"#
    );
    assert_eq!(hit.header("content-type"), Some("application/json"));
    assert_eq!(
        exchange.calls[0].outcome,
        CallOutcome::Result(json!("0x10"))
    );
    assert_eq!(exchange.http.as_ref().unwrap().status, 200);
}

#[tokio::test]
async fn an_error_object_fails_the_call() {
    let server = JsonrpcHttpServer::start(|req, _| {
        RpcReply::json(&error(req.json()["id"].clone(), -32601, "Method not found"))
    })
    .await;
    let exchange = send(
        &target(&server.url("/"), vec![call("nope")], false),
        &SendOptions::default(),
    )
    .await;
    assert!(exchange.error.is_none());
    assert!(!exchange.passed());
    assert_eq!(exchange.failures(), ["rpc error -32601: Method not found"]);
    let CallOutcome::Error(e) = &exchange.calls[0].outcome else {
        panic!("an error outcome");
    };
    assert_eq!((e.code, e.message.as_str()), (-32601, "Method not found"));
}

#[tokio::test]
async fn a_gateway_error_page_is_http_502() {
    let server = JsonrpcHttpServer::start(|_, _| {
        RpcReply::raw(502, "text/html", "<html>Bad Gateway</html>")
    })
    .await;
    let exchange = send(
        &target(&server.url("/"), vec![call("m")], false),
        &SendOptions::default(),
    )
    .await;
    assert_eq!(exchange.error, Some(JsonrpcError::Http(502)));
    assert_eq!(exchange.error.unwrap().to_string(), "HTTP 502");
    assert_eq!(
        exchange.received.as_deref(),
        Some("<html>Bad Gateway</html>")
    );
}

#[tokio::test]
async fn a_node_that_does_not_answer_times_out() {
    let server = JsonrpcHttpServer::start(|req, _| {
        RpcReply::json(&echo_result(&req.json())).delayed(Duration::from_secs(20))
    })
    .await;
    let exchange = send(
        &target(&server.url("/"), vec![call("m")], false),
        &timeout_ms(400),
    )
    .await;
    let err = exchange.error.expect("a timeout");
    assert_eq!(err.to_string(), "timeout after 400 ms (response)");
}

#[tokio::test]
async fn a_mismatched_id_is_an_exchange_error() {
    let server = JsonrpcHttpServer::start(|_, _| RpcReply::json(&result(json!(2), json!(1)))).await;
    let exchange = send(
        &target(&server.url("/"), vec![call("m")], false),
        &SendOptions::default(),
    )
    .await;
    assert_eq!(
        exchange.error.unwrap().to_string(),
        "response id 2 does not match request id 1"
    );
}

#[tokio::test]
async fn a_malformed_response_is_an_exchange_error() {
    let both = json!({"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":"m"}});
    let server = JsonrpcHttpServer::start(move |_, _| RpcReply::json(&both)).await;
    let exchange = send(
        &target(&server.url("/"), vec![call("m")], false),
        &SendOptions::default(),
    )
    .await;
    assert_eq!(
        exchange.error.unwrap().to_string(),
        "invalid JSON-RPC response: both result and error present"
    );
}

#[tokio::test]
async fn a_notification_completes_on_204() {
    let server = JsonrpcHttpServer::start(|_, _| RpcReply::empty(204)).await;
    let mut notify = call("log");
    notify.notification = true;
    let exchange = send(
        &target(&server.url("/"), vec![notify], false),
        &SendOptions::default(),
    )
    .await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    assert_eq!(exchange.calls[0].outcome, CallOutcome::Notification);
    assert_eq!(
        server.hits()[0].json(),
        json!({"jsonrpc": "2.0", "method": "log"})
    );
}

#[tokio::test]
async fn a_two_call_batch_is_one_request_matched_out_of_order() {
    let server = JsonrpcHttpServer::start(|_, _| {
        RpcReply::json(&json!([
            result(json!(2), json!("0x10")),
            result(json!(1), json!("0x1"))
        ]))
    })
    .await;
    let batch = target(
        &server.url("/"),
        vec![call("eth_chainId"), call("eth_blockNumber")],
        true,
    );
    let exchange = send(&batch, &SendOptions::default()).await;
    let hits = server.hits();
    assert_eq!(hits.len(), 1);
    let sent = hits[0].json();
    assert_eq!(
        (sent[0]["id"].clone(), sent[1]["id"].clone()),
        (json!(1), json!(2))
    );
    assert!(exchange.passed());
    assert_eq!(exchange.calls[0].outcome, CallOutcome::Result(json!("0x1")));
    assert_eq!(
        exchange.calls[1].outcome,
        CallOutcome::Result(json!("0x10"))
    );
    assert_eq!(exchange.envelope(1)["result"], "0x10");
}

/// Regression: an auto id once reused an explicit one, and both calls
/// were matched to the same answer.
#[tokio::test]
async fn an_auto_id_never_reuses_an_explicit_one() {
    let server = JsonrpcHttpServer::start(|req, _| {
        let answers: Vec<Value> = req
            .json()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| result(c["id"].clone(), c["method"].clone()))
            .collect();
        RpcReply::json(&Value::Array(answers))
    })
    .await;
    let mut first = call("first");
    first.id = Some(json!(1));
    let batch = target(&server.url("/"), vec![first, call("second")], true);
    let exchange = send(&batch, &SendOptions::default()).await;
    let sent = server.hits()[0].json();
    assert_eq!(
        (sent[0]["id"].clone(), sent[1]["id"].clone()),
        (json!(1), json!(2))
    );
    assert_eq!(
        exchange.calls[0].outcome,
        CallOutcome::Result(json!("first"))
    );
    assert_eq!(
        exchange.calls[1].outcome,
        CallOutcome::Result(json!("second"))
    );
}

#[tokio::test]
async fn a_missing_batch_item_fails_that_call() {
    let server =
        JsonrpcHttpServer::start(|_, _| RpcReply::json(&json!([result(json!(1), json!("0x1"))])))
            .await;
    let batch = target(&server.url("/"), vec![call("a"), call("b")], true);
    let exchange = send(&batch, &SendOptions::default()).await;
    assert!(!exchange.passed());
    assert!(exchange.calls[0].failure().is_none());
    assert_eq!(
        exchange.calls[1].failure().as_deref(),
        Some("no response for id 2")
    );
}

#[tokio::test]
async fn a_server_rejecting_batches_fails_every_call_once() {
    let server = JsonrpcHttpServer::start(|_, _| {
        RpcReply::json(&error(Value::Null, -32600, "batch requests are disabled"))
    })
    .await;
    let batch = target(&server.url("/"), vec![call("a"), call("b")], true);
    let exchange = send(&batch, &SendOptions::default()).await;
    assert!(exchange.calls.iter().all(|c| c.failure().is_some()));
    assert_eq!(
        exchange.failures(),
        ["rpc error -32600: batch requests are disabled (server rejected the batch)"]
    );
}

#[tokio::test]
async fn a_rate_limited_idempotent_read_is_retried() {
    let server = JsonrpcHttpServer::start(|req, n| match n {
        0 => RpcReply::raw(429, "text/plain", "slow down").with_header("retry-after", "0"),
        _ => RpcReply::json(&echo_result(&req.json())),
    })
    .await;
    let read = idempotent(
        target(&server.url("/"), vec![call("getblockcount")], false),
        2,
    );
    let exchange = send(&read, &SendOptions::default()).await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    assert_eq!(server.hits().len(), 2);
    let statuses: Vec<_> = exchange.attempts.iter().map(|a| a.status).collect();
    assert_eq!(statuses, [Some(429), Some(200)]);
    assert_eq!(exchange.attempts[0].error.as_deref(), Some("HTTP 429"));
    assert_eq!(
        exchange.attempts[0].retry_in,
        Some(Duration::ZERO),
        "Retry-After: 0 wins"
    );
}

#[tokio::test]
async fn limit_exceeded_errors_are_retried() {
    let server = JsonrpcHttpServer::start(|req, n| match n {
        0 => RpcReply::json(&error(req.json()["id"].clone(), -32005, "limit exceeded")),
        _ => RpcReply::json(&echo_result(&req.json())),
    })
    .await;
    let read = idempotent(
        target(&server.url("/"), vec![call("eth_getLogs")], false),
        1,
    );
    let exchange = send(&read, &SendOptions::default()).await;
    assert!(exchange.passed());
    assert_eq!(exchange.attempts.len(), 2);
    assert_eq!(
        exchange.attempts[0].error.as_deref(),
        Some("rpc error -32005: limit exceeded")
    );
}

#[tokio::test]
async fn a_non_idempotent_call_is_not_retried() {
    let server = JsonrpcHttpServer::start(|req, _| {
        RpcReply::json(&echo_result(&req.json())).delayed(Duration::from_secs(20))
    })
    .await;
    let mut write = target(&server.url("/"), vec![call("sendrawtransaction")], false);
    write.retry.attempts = Some(2);
    let exchange = send(&write, &timeout_ms(300)).await;
    assert_eq!(exchange.attempts.len(), 1);
    assert_eq!(server.hits().len(), 1);
    assert_eq!(
        exchange.error.unwrap().to_string(),
        "timeout after 300 ms (response)"
    );
}

#[tokio::test]
async fn retries_stop_after_the_last_attempt() {
    let server = JsonrpcHttpServer::start(|_, _| RpcReply::raw(503, "text/plain", "busy")).await;
    let read = idempotent(target(&server.url("/"), vec![call("m")], false), 2);
    let exchange = send(&read, &SendOptions::default()).await;
    assert_eq!(server.hits().len(), 3);
    assert_eq!(exchange.attempts.len(), 3);
    let waits: Vec<_> = exchange.attempts.iter().map(|a| a.retry_in).collect();
    assert_eq!(
        waits,
        [
            Some(Duration::from_millis(10)),
            Some(Duration::from_millis(20)),
            None
        ]
    );
    assert_eq!(exchange.error, Some(JsonrpcError::Http(503)));
}

#[tokio::test]
async fn subscriptions_are_refused_over_http() {
    let mut sub = target("http://127.0.0.1:9/", vec![call("eth_subscribe")], false);
    sub.subscription = Some(rustman_core::model::SubscriptionSpec {
        subscribe: "eth_subscribe".into(),
        unsubscribe: "eth_unsubscribe".into(),
        notification: "eth_subscription".into(),
        notification_id_path: None,
        max_events: None,
        max_duration_ms: None,
    });
    let exchange = send(&sub, &SendOptions::default()).await;
    assert_eq!(
        exchange.error.unwrap().to_string(),
        "subscriptions require a ws:// or wss:// endpoint"
    );
}

/// Options whose in-memory body cap (`[http] max_body_bytes`) is `limit`.
fn capped(limit: u64) -> SendOptions {
    let mut options = SendOptions::default();
    options.body.max_in_memory = limit;
    options
}

/// Regression: a body over `max_body_bytes` was spilled to disk, read back
/// whole, parsed and recorded. It is refused unread, and never retried.
#[tokio::test]
async fn a_body_over_max_body_bytes_is_refused_unread() {
    let big = "y".repeat(64 * 1024);
    let reply = result(json!(1), json!(big)).to_string();
    let server =
        JsonrpcHttpServer::start(move |_, _| RpcReply::raw(200, "application/json", &reply)).await;
    let read = idempotent(target(&server.url("/"), vec![call("big")], false), 2);
    let exchange = send(&read, &capped(4096)).await;
    assert_eq!(
        exchange.error.as_ref().map(ToString::to_string).as_deref(),
        Some("HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)")
    );
    assert_eq!(server.hits().len(), 1, "an oversized answer is not retried");
    assert!(exchange.calls.is_empty());
    assert!(exchange.received.is_none());
    assert!(exchange.http.is_none());
}

#[tokio::test]
async fn a_body_of_exactly_max_body_bytes_is_read() {
    let reply = result(json!(1), json!("z".repeat(1000))).to_string();
    let size = reply.len() as u64;
    let served = reply.clone();
    let server =
        JsonrpcHttpServer::start(move |_, _| RpcReply::raw(200, "application/json", &served)).await;
    let one = target(&server.url("/"), vec![call("m")], false);
    let exchange = send(&one, &capped(size)).await;
    assert!(exchange.passed(), "{:?}", exchange.error);
    assert_eq!(exchange.received.as_deref(), Some(reply.as_str()));
    let exchange = send(&one, &capped(size - 1)).await;
    assert!(
        matches!(exchange.error, Some(JsonrpcError::TooLarge(_))),
        "{:?}",
        exchange.error
    );
}

/// A traced call carries `traceparent` on its HTTP POST
/// (add-trace-propagation task 2.3).
#[tokio::test]
async fn a_traced_call_carries_a_traceparent() {
    let server = JsonrpcHttpServer::start(|req, _| RpcReply::json(&echo_result(&req.json()))).await;
    let options = SendOptions {
        trace: Some(rustman_core::trace::TracePropagation::default()),
        ..SendOptions::default()
    };
    let target = target(&server.url("/"), vec![call("eth_blockNumber")], false);
    assert!(send(&target, &options).await.passed());
    assert!(send(&target, &SendOptions::default()).await.passed());
    let hits = server.hits();
    let traceparent = hits[0].header("traceparent").expect("a traceparent");
    assert!(traceparent.starts_with("00-") && traceparent.ends_with("-01"));
    assert_eq!(traceparent.len(), 55);
    assert_eq!(hits[1].header("traceparent"), None);
}
