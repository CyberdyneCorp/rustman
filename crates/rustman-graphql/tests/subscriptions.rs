//! Subscriptions (add-graphql-client tasks 5.1 to 5.4) against in-process
//! fake servers: `graphql-transport-ws`, the legacy `graphql-ws`, GraphQL
//! over SSE, and the headless bounds.

use std::fs;
use std::time::Duration;

use rustman_core::Workspace;
use rustman_core::model::Request;
use rustman_core::vars::{Resolver, SecretLookup, resolve_graphql};
use rustman_graphql::{
    DEFAULT_ACK_TIMEOUT, Delivery, Ended, Executed, Execution, GraphqlError, Limits, ResultStatus,
    SubscribeOptions, Subscription, WireProtocol, execute,
};
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::{GraphqlSseServer, GraphqlWsServer, SseEnd, SseScript, WsScript};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const SECRET: &str = "s3cr3t-subscription-canary";
const SUBSCRIPTION: &str =
    "subscription OnMessage($room: String!) { onMessage(room: $room) { n } }";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    /// A workspace whose `base` variable is `base`, with one subscription
    /// request `api/s.rman.yaml` whose extra fields are `extra`.
    fn new(base: &str, extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "rustman.toml",
            &format!(
                "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{base}\"\nroom = \"lobby\"\n"
            ),
        );
        write("environments/prod.yaml", "secrets: [api_token]\n");
        write(
            "api/s.rman.yaml",
            &format!(
                "version: 1\nkind: graphql\nname: s\nendpoint: '{{{{base}}}}/graphql'\n\
                 document: '{SUBSCRIPTION}'\nvariables:\n  room: '{{{{room}}}}'\n\
                 auth:\n  type: bearer\n  token: '{{{{api_token}}}}'\n{extra}"
            ),
        );
        Self { dir }
    }

    fn workspace(&self) -> Workspace {
        Workspace::load(self.dir.path()).unwrap_or_else(|e| panic!("{e}"))
    }
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| SECRET.to_owned()))
    }
}

/// Send the fixture's request headless (the request's limits), or with
/// `limits` and `ack_timeout` when given.
async fn subscribe(
    f: &Fixture,
    limits: Option<Limits>,
    ack_timeout: Option<Duration>,
    cancel: CancellationToken,
) -> Result<Executed, GraphqlError> {
    subscribe_with(f, limits, ack_timeout, cancel, &SendOptions::default()).await
}

/// [`subscribe`] with the transport options `options`.
async fn subscribe_with(
    f: &Fixture,
    limits: Option<Limits>,
    ack_timeout: Option<Duration>,
    cancel: CancellationToken,
    options: &SendOptions,
) -> Result<Executed, GraphqlError> {
    let ws = f.workspace();
    let request = ws.find_by_name("s").remove(0);
    let Request::Graphql(graphql) = request.request() else {
        panic!("a graphql request");
    };
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, &request, env, &Store);
    let resolved = resolve_graphql(&request, &resolver).unwrap();
    let limits = limits.unwrap_or_else(|| Limits::headless(graphql, None, None));
    let mut subscribe = SubscribeOptions::new(limits, cancel);
    subscribe.ack_timeout = ack_timeout.unwrap_or(DEFAULT_ACK_TIMEOUT);
    let engine = HttpEngine::new();
    let exec = Execution {
        request: &request,
        graphql,
        resolved: &resolved,
        engine: &engine,
        options,
        root: &ws.root,
        settings: &ws.settings,
        validate: true,
        subscribe,
    };
    execute(&exec).await
}

async fn headless(f: &Fixture) -> Result<Subscription, GraphqlError> {
    let executed = subscribe(f, None, None, CancellationToken::new()).await?;
    match executed.delivery {
        Delivery::Subscription(sub) => Ok(*sub),
        Delivery::Http(_) => panic!("a subscription"),
    }
}

fn numbers(sub: &Subscription) -> Vec<u64> {
    sub.events
        .iter()
        .map(|e| e.payload["data"]["onMessage"]["n"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn three_next_messages_are_three_timestamped_events_in_order() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 3,
        complete: true,
    })
    .await;
    let extra = "transport: ws\nconnection_params:\n  authToken: '{{api_token}}'\n";
    let f = Fixture::new(&server.url("ws", ""), extra);
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1, 2, 3]);
    assert_eq!(sub.ended, Ended::Complete);
    assert_eq!(sub.protocol.label(), "graphql-transport-ws");
    assert_eq!(sub.status, 101);
    assert!(sub.events.windows(2).all(|w| w[0].elapsed <= w[1].elapsed));
    assert!(sub.events.iter().all(|e| e.ts.ends_with('Z')));
    assert_eq!(
        sub.events[0].payload["data"]["onMessage"]["vars"],
        json!({"room": "lobby"})
    );
    assert_eq!(sub.request.method, "GET");
    assert!(
        sub.request.url.text.starts_with("ws://"),
        "{}",
        sub.request.url.text
    );

    let session = &server.sessions()[0];
    assert_eq!(session.offered, ["graphql-transport-ws", "graphql-ws"]);
    assert_eq!(
        session.header("authorization"),
        Some(&*format!("Bearer {SECRET}"))
    );
    assert_eq!(session.message_types(), ["connection_init", "subscribe"]);
    assert_eq!(session.messages[0]["payload"], json!({"authToken": SECRET}));
    let payload = &session.messages[1]["payload"];
    assert_eq!(payload["query"], SUBSCRIPTION);
    assert_eq!(payload["variables"], json!({"room": "lobby"}));
}

#[tokio::test]
async fn http_endpoints_are_reached_over_ws() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: true,
    })
    .await;
    let f = Fixture::new(&server.url("http", ""), "transport: ws\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1]);
}

#[tokio::test]
async fn a_close_code_fails_the_subscription_with_its_reason() {
    let server = GraphqlWsServer::start(WsScript::Close {
        code: 4403,
        before_ack: false,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(sub.ended, Ended::Closed);
    let failure = sub.failure.clone().expect("a failure");
    assert!(
        failure.starts_with("the server closed the connection without complete (4403 Forbidden"),
        "{failure}"
    );
    assert_eq!(sub.status(), ResultStatus::Errors);
    assert_eq!(sub.result()["errors"][0]["message"], json!(failure));
}

#[tokio::test]
async fn a_close_before_ack_keeps_its_code_and_hides_secrets() {
    let server = GraphqlWsServer::start(WsScript::Close {
        code: 4401,
        before_ack: true,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let err = headless(&f).await.unwrap_err().to_string();
    assert!(
        err.contains("before connection_ack (4401 Forbidden: Bearer "),
        "{err}"
    );
    assert!(!err.contains(SECRET), "{err}");
}

#[tokio::test]
async fn an_sse_stream_ending_without_complete_fails() {
    let server = GraphqlSseServer::start(SseScript {
        events: 2,
        end: SseEnd::Close,
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1, 2]);
    assert_eq!(sub.ended, Ended::Closed);
    assert_eq!(
        sub.failure.as_deref(),
        Some("SSE: the stream ended without a complete event")
    );
    assert_eq!(sub.status(), ResultStatus::Partial);
}

#[tokio::test]
async fn a_server_that_never_acknowledges_times_out() {
    assert_eq!(DEFAULT_ACK_TIMEOUT, Duration::from_secs(10));
    let server = GraphqlWsServer::start(WsScript::NoAck).await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let ack = Some(Duration::from_millis(300));
    let err = subscribe(&f, None, ack, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(err, GraphqlError::AckTimeout);
    assert_eq!(err.to_string(), "graphql: connection_ack timeout");
    assert_eq!(server.sessions()[0].message_types(), ["connection_init"]);
}

#[tokio::test]
async fn stopping_a_subscription_completes_it_on_the_server() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: false,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        stop.cancel();
    });
    let executed = subscribe(&f, Some(Limits::interactive()), None, cancel)
        .await
        .unwrap();
    let Delivery::Subscription(sub) = executed.delivery else {
        panic!("a subscription");
    };
    assert_eq!(sub.ended, Ended::Stopped);
    assert_eq!(numbers(&sub), [1]);
    // The server records the client's `complete` once it reads it.
    let completed = || {
        server.sessions()[0]
            .message_types()
            .contains(&"complete".to_owned())
    };
    for _ in 0..100 {
        if completed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed(), "{:?}", server.sessions()[0].message_types());
}

#[tokio::test]
async fn a_legacy_only_server_is_used_with_its_protocol() {
    let server = GraphqlWsServer::start(WsScript::Legacy { events: 2 }).await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1, 2]);
    assert_eq!(sub.ended, Ended::Complete);
    assert_eq!(sub.protocol.label(), "legacy subscriptions-transport-ws");
    let session = &server.sessions()[0];
    assert_eq!(session.chosen.as_deref(), Some("graphql-ws"));
    assert_eq!(session.message_types(), ["connection_init", "start"]);
}

#[tokio::test]
async fn ws_protocol_forces_one_protocol() {
    let server = GraphqlWsServer::start(WsScript::Legacy { events: 1 }).await;
    let url = server.url("ws", "");
    let forced = Fixture::new(&url, "transport: ws\nws_protocol: graphql-ws\n");
    let sub = headless(&forced).await.unwrap();
    assert_eq!(
        sub.protocol,
        rustman_graphql::SubscriptionProtocol::Ws(WireProtocol::Legacy)
    );
    assert_eq!(server.sessions()[0].offered, ["graphql-ws"]);

    let modern = Fixture::new(&url, "transport: ws\nws_protocol: graphql-transport-ws\n");
    let err = headless(&modern).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "graphql: subscription: WebSocket handshake failed: HTTP 400"
    );
    assert_eq!(server.sessions()[1].offered, ["graphql-transport-ws"]);
}

#[tokio::test]
async fn sse_records_each_next_event_until_complete() {
    let server = GraphqlSseServer::start(SseScript {
        events: 3,
        end: SseEnd::Complete,
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1, 2, 3]);
    assert_eq!(sub.ended, Ended::Complete);
    assert_eq!(sub.protocol.label(), "sse");
    assert_eq!(sub.status, 200);
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.header("accept"), Some("text/event-stream"));
    assert_eq!(
        hit.header("authorization"),
        Some(&*format!("Bearer {SECRET}"))
    );
    assert_eq!(hit.json()["query"], SUBSCRIPTION);
    assert_eq!(hit.json()["variables"], json!({"room": "lobby"}));
}

#[tokio::test]
async fn sse_complete_without_a_data_line_ends_the_subscription() {
    let server = GraphqlSseServer::start(SseScript {
        events: 2,
        end: SseEnd::CompleteWithoutData,
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), [1, 2]);
    assert_eq!(sub.ended, Ended::Complete);
}

#[tokio::test]
async fn headless_ws_subscriptions_stop_at_ten_of_fifty_events() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 50,
        complete: false,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(numbers(&sub), (1..=10).collect::<Vec<u64>>());
    assert_eq!(sub.ended, Ended::MaxEvents);
}

#[tokio::test]
async fn headless_sse_subscriptions_stop_at_ten_of_fifty_events() {
    let server = GraphqlSseServer::start(SseScript {
        events: 50,
        end: SseEnd::Open,
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(sub.events.len(), 10);
    assert_eq!(sub.ended, Ended::MaxEvents);
}

#[tokio::test]
async fn request_limits_and_overrides_bound_headless_runs() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 50,
        complete: false,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\nmax_events: 3\n");
    assert_eq!(headless(&f).await.unwrap().events.len(), 3);

    let quiet = GraphqlWsServer::start(WsScript::Modern {
        events: 0,
        complete: false,
    })
    .await;
    let f = Fixture::new(&quiet.url("ws", ""), "transport: ws\nduration: 300ms\n");
    let sub = headless(&f).await.unwrap();
    assert_eq!(sub.ended, Ended::Duration);
    assert!(
        sub.elapsed >= Duration::from_millis(300),
        "{:?}",
        sub.elapsed
    );
}

/// A server that accepts connections and never answers; its URL.
async fn silent_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            held.push(tcp);
        }
    });
    (url, task)
}

#[tokio::test]
async fn a_silent_sse_server_does_not_outlast_the_duration() {
    let (url, server) = silent_server().await;
    let f = Fixture::new(&url, "transport: sse\nduration: 300ms\n");
    let err = tokio::time::timeout(Duration::from_secs(20), headless(&f))
        .await
        .expect("bounded by the duration, not the 30 s HTTP timeout")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("SSE: the stream did not open within"),
        "{err}"
    );
    server.abort();
}

#[tokio::test]
async fn stopping_an_sse_subscription_that_has_not_opened_ends_it() {
    let (url, server) = silent_server().await;
    let f = Fixture::new(&url, "transport: sse\n");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop.cancel();
    });
    let opened = subscribe(&f, Some(Limits::interactive()), None, cancel);
    let err = tokio::time::timeout(Duration::from_secs(20), opened)
        .await
        .expect("the stop ends it")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("SSE: stopped before the stream opened"),
        "{err}"
    );
    server.abort();
}

#[tokio::test]
async fn subscriptions_are_validated_before_connecting() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: true,
    })
    .await;
    let dir = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    fs::write(
        dir.dir.path().join("api/s.rman.yaml"),
        format!(
            "version: 1\nkind: graphql\nname: s\nendpoint: '{}'\ntransport: ws\ndocument: 'subscription {{ a'\n",
            server.url("ws", "/graphql")
        ),
    )
    .unwrap();
    let err = headless(&dir).await.unwrap_err();
    assert!(
        err.to_string()
            .starts_with("graphql: the document is invalid: 1:"),
        "{err}"
    );
    assert!(server.sessions().is_empty());
}

#[tokio::test]
async fn an_sse_event_is_bounded_by_the_in_memory_body_cap() {
    let server = GraphqlSseServer::start(SseScript {
        events: 1,
        end: SseEnd::Unended { bytes: 1024 * 1024 },
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let mut options = SendOptions::default();
    options.body.max_in_memory = 256 * 1024;
    assert!(options.body.max_download > 1024 * 1024 * 1024 - 1);
    let executed = subscribe_with(&f, None, None, CancellationToken::new(), &options)
        .await
        .unwrap();
    let Delivery::Subscription(sub) = executed.delivery else {
        panic!("a subscription");
    };
    assert_eq!(numbers(&sub), [1]);
    assert_eq!(sub.ended, Ended::Error);
    assert_eq!(
        sub.failure.as_deref(),
        Some("SSE: an event is larger than the 262144 byte limit (max_body_bytes)")
    );
}

#[tokio::test]
async fn a_websocket_message_is_bounded_by_the_in_memory_body_cap() {
    let server = GraphqlWsServer::start(WsScript::Large { bytes: 256 * 1024 }).await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let mut options = SendOptions::default();
    options.body.max_in_memory = 64 * 1024;
    let executed = subscribe_with(&f, None, None, CancellationToken::new(), &options)
        .await
        .unwrap();
    let Delivery::Subscription(sub) = executed.delivery else {
        panic!("a subscription");
    };
    assert_eq!(numbers(&sub), [1]);
    assert_eq!(sub.ended, Ended::Error);
    assert_eq!(
        sub.failure.as_deref(),
        Some("WebSocket: a message is larger than the 65536 byte limit (max_body_bytes)")
    );
}

#[tokio::test]
async fn the_duration_also_bounds_the_wait_for_connection_ack() {
    let server = GraphqlWsServer::start(WsScript::NoAck).await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\nduration: 300ms\n");
    let started = std::time::Instant::now();
    // The ack timeout (10 s) is longer than the duration.
    let err = subscribe(&f, None, None, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(err, GraphqlError::AckTimeout);
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(5), "{waited:?}");
}

/// Options that inject W3C trace headers (add-trace-propagation task 2.3).
fn traced() -> SendOptions {
    SendOptions {
        trace: Some(rustman_core::trace::TracePropagation::default()),
        ..SendOptions::default()
    }
}

fn assert_traceparent(value: Option<&str>) {
    let value = value.expect("a traceparent");
    assert!(
        value.starts_with("00-") && value.ends_with("-01"),
        "{value}"
    );
    assert_eq!(value.len(), 55, "{value}");
}

#[tokio::test]
async fn a_traced_websocket_upgrade_carries_a_traceparent() {
    let server = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: true,
    })
    .await;
    let f = Fixture::new(&server.url("ws", ""), "transport: ws\n");
    let executed = subscribe_with(&f, None, None, CancellationToken::new(), &traced())
        .await
        .unwrap();
    assert_traceparent(server.sessions()[0].header("traceparent"));
    assert_recorded(&executed, server.sessions()[0].header("traceparent"));
    let plain = GraphqlWsServer::start(WsScript::Modern {
        events: 1,
        complete: true,
    })
    .await;
    let f = Fixture::new(&plain.url("ws", ""), "transport: ws\n");
    headless(&f).await.unwrap();
    assert_eq!(plain.sessions()[0].header("traceparent"), None);
}

#[tokio::test]
async fn a_traced_sse_subscription_post_carries_a_traceparent() {
    let server = GraphqlSseServer::start(SseScript {
        events: 1,
        end: SseEnd::Complete,
    })
    .await;
    let f = Fixture::new(&server.url(""), "transport: sse\n");
    let executed = subscribe_with(&f, None, None, CancellationToken::new(), &traced())
        .await
        .unwrap();
    assert_traceparent(server.hits()[0].header("traceparent"));
    assert_recorded(&executed, server.hits()[0].header("traceparent"));
}

/// Issue #119: the subscription keeps the context its opening request
/// carried, and so does the exchange runs record.
fn assert_recorded(executed: &Executed, sent: Option<&str>) {
    let Delivery::Subscription(sub) = &executed.delivery else {
        panic!("a subscription");
    };
    let trace = sub.trace.as_ref().expect("the opening request's trace");
    let ids = format!("00-{}-{}-01", trace.context.trace_id, trace.context.span_id);
    assert_eq!(Some(ids.as_str()), sent);
    let exchange = rustman_graphql::run::subscription_exchange(sub);
    assert_eq!(exchange.trace.as_ref(), Some(trace));
}
