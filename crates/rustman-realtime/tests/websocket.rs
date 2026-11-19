//! WebSocket sessions against the in-process test server
//! (add-realtime-client tasks 2.1 to 2.4): the handshake and subprotocols,
//! a refused upgrade, TLS and proxies, every frame type, ping/pong,
//! closing from either side and the steps interpreter.

use std::time::Duration;

use rustman_core::Secret;
use rustman_core::model::{Checked, RequestSettings};
use rustman_core::model::{CloseStep, JsonAssertion, RealtimeLimits, WaitFor};
use rustman_core::vars::{
    Resolved, ResolvedBinary, ResolvedBody, ResolvedFile, ResolvedFrame, ResolvedRequest,
    ResolvedStep, ResolvedWs, TlsFiles,
};
use rustman_http::{ProxySettings, SendOptions, TlsSettings};
use rustman_realtime::{
    Bounds, Direction, RealtimeExchange, SessionOptions, TimelineEntry, run_ws,
};
use rustman_test_support::server::Tls;
use rustman_test_support::{RealtimeWsServer, TestCerts, TestProxy, WsAction, WsBehavior};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

fn endpoint(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "GET".to_owned(),
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

fn target(url: &str, steps: Vec<ResolvedStep>) -> ResolvedWs {
    ResolvedWs {
        http: endpoint(url),
        subprotocols: Vec::new(),
        steps,
        limits: RealtimeLimits::default(),
    }
}

/// Headless, with room for the test's steps: stop after `messages`
/// received messages or 10 s.
fn headless(messages: u64) -> SessionOptions {
    SessionOptions::headless(
        Bounds {
            max_messages: messages,
            duration: Duration::from_secs(10),
        },
        CancellationToken::new(),
    )
}

async fn run(target: &ResolvedWs, options: &SendOptions) -> RealtimeExchange {
    run_ws(target, options, headless(1_000)).await
}

fn close() -> ResolvedStep {
    ResolvedStep::Close(CloseStep::default())
}

fn text(t: &str) -> ResolvedStep {
    ResolvedStep::Send(ResolvedFrame::Text(t.to_owned()))
}

fn sleep(ms: u64) -> ResolvedStep {
    ResolvedStep::Sleep(ms)
}

fn wait_regex(pattern: &str, timeout_ms: u64) -> ResolvedStep {
    ResolvedStep::WaitFor(Box::new(WaitFor {
        json: None,
        regex: Some(pattern.to_owned()),
        timeout_ms: Some(timeout_ms),
    }))
}

fn entries(exchange: &RealtimeExchange, dir: Direction, kind: &str) -> Vec<TimelineEntry> {
    exchange
        .timeline
        .entries()
        .filter(|e| e.dir == dir && e.kind == kind)
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_server_picks_a_subprotocol_from_the_ordered_offer() {
    let server = RealtimeWsServer::start(WsBehavior::default().accept(&["v1.feed"])).await;
    let mut session = target(&server.url("/feed"), vec![close()]);
    session.subprotocols = vec!["v2.feed".into(), "v1.feed".into()];
    let exchange = run(&session, &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.subprotocol(), Some("v1.feed"));
    let handshake = exchange.handshake.as_ref().unwrap();
    assert_eq!(handshake.status, 101);
    assert_eq!(handshake.header("sec-websocket-protocol"), Some("v1.feed"));
    assert_eq!(server.sessions()[0].offered, ["v2.feed", "v1.feed"]);
    assert_eq!(exchange.request.url.text, server.url("/feed"));
    let connect = entries(&exchange, Direction::System, "connect");
    assert_eq!(connect[0].payload, "connected (subprotocol v1.feed)");
}

#[tokio::test]
async fn a_refused_upgrade_keeps_status_and_body() {
    let server = RealtimeWsServer::start(WsBehavior::default().refuse(401, "token required")).await;
    let exchange = run(&target(&server.url("/"), vec![]), &SendOptions::default()).await;
    let error = exchange.error.as_ref().expect("refused");
    assert_eq!(error.to_string(), "ws: handshake failed: 401 Unauthorized");
    let handshake = exchange.handshake.as_ref().unwrap();
    assert_eq!(handshake.status, 401);
    assert_eq!(handshake.body.as_deref(), Some("token required"));
}

#[tokio::test]
async fn handshake_headers_are_sent() {
    let server = RealtimeWsServer::start(WsBehavior::default()).await;
    let mut session = target(&server.url("/"), vec![close()]);
    session.http.headers.push(rustman_core::vars::ResolvedPair {
        name: Resolved::literal("Origin"),
        value: Resolved::literal("https://app.test"),
    });
    let exchange = run(&session, &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    assert_eq!(
        server.sessions()[0].header("origin"),
        Some("https://app.test")
    );
}

#[tokio::test]
async fn a_self_signed_certificate_is_rejected_naming_the_host() {
    let certs = TestCerts::generate();
    let server = RealtimeWsServer::start_tls(WsBehavior::default(), &certs, Tls::SelfSigned).await;
    let exchange = run(
        &target(&server.localhost_url("/"), vec![]),
        &SendOptions::default(),
    )
    .await;
    let error = exchange.error.expect("rejected").to_string();
    assert!(error.starts_with("ws: tls: "), "{error}");
    assert!(error.contains("localhost"), "{error}");
    assert!(server.sessions().is_empty() || server.sessions()[0].messages.is_empty());
}

#[tokio::test]
async fn a_custom_ca_is_trusted() {
    let certs = TestCerts::generate();
    let server = RealtimeWsServer::start_tls(WsBehavior::echo(), &certs, Tls::Http1Only).await;
    let options = SendOptions {
        tls: TlsSettings {
            ca_bundle: Some(certs.ca_pem.clone().into_bytes()),
            ..TlsSettings::default()
        },
        ..SendOptions::default()
    };
    let steps = vec![text("over tls"), wait_regex("over tls", 5000), close()];
    let exchange = run(&target(&server.localhost_url("/"), steps), &options).await;
    assert_eq!(exchange.error, None);
    assert_eq!(server.sessions()[0].texts(), ["over tls"]);
}

#[tokio::test]
async fn an_http_proxy_tunnels_the_session_with_connect() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let proxy = TestProxy::http().await;
    let options = SendOptions {
        proxy: ProxySettings::explicit(&proxy.url()).unwrap(),
        ..SendOptions::default()
    };
    let steps = vec![text("via proxy"), wait_regex("via proxy", 5000), close()];
    let exchange = run(&target(&server.url("/"), steps), &options).await;
    assert_eq!(exchange.error, None);
    let connect = format!("CONNECT {} HTTP/1.1", server.authority());
    assert!(
        proxy.log().iter().any(|l| l.starts_with(&connect)),
        "{:?}",
        proxy.log()
    );
}

#[tokio::test]
async fn text_json_and_binary_frames_are_sent() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("blob.bin");
    std::fs::write(&blob, [0xde, 0xad, 0xbe, 0xef]).unwrap();
    let server = RealtimeWsServer::start(WsBehavior::default()).await;
    let steps = vec![
        text("hello"),
        ResolvedStep::Send(ResolvedFrame::Json(
            json!({"op": "subscribe", "channel": "ticker"}),
        )),
        ResolvedStep::Send(ResolvedFrame::Binary(ResolvedBinary::Base64("AAEC".into()))),
        ResolvedStep::Send(ResolvedFrame::Binary(ResolvedBinary::File(ResolvedFile {
            raw: "blob.bin".into(),
            path: blob,
        }))),
        close(),
    ];
    let exchange = run(&target(&server.url("/"), steps), &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    let received = &server.sessions()[0].messages;
    assert_eq!(received[0], Message::text("hello"));
    assert_eq!(
        received[1],
        Message::text(r#"{"op":"subscribe","channel":"ticker"}"#)
    );
    assert_eq!(received[2], Message::binary(vec![0u8, 1, 2]));
    assert_eq!(
        received[3],
        Message::binary(vec![0xde_u8, 0xad, 0xbe, 0xef])
    );
    let sent: Vec<(String, u64)> = exchange
        .timeline
        .entries()
        .filter(|e| e.dir == Direction::Sent)
        .map(|e| (e.kind.clone(), e.size))
        .collect();
    assert_eq!(
        sent,
        [
            ("text".to_owned(), 5),
            ("json".to_owned(), 37),
            ("binary".to_owned(), 3),
            ("binary".to_owned(), 4),
            ("close".to_owned(), 2),
        ]
    );
}

#[tokio::test]
async fn server_pings_are_answered_and_client_pings_time_their_pong() {
    // The server answers our ping with a text after its pong, so waiting
    // for the text (not a fixed sleep) sees the pong.
    let behavior = WsBehavior::default()
        .on_connect(vec![WsAction::Ping(b"srv".to_vec())])
        .reply(|message| match message {
            Message::Ping(_) => vec![WsAction::Text("pinged".into())],
            _ => Vec::new(),
        });
    let server = RealtimeWsServer::start(behavior).await;
    let steps = vec![
        ResolvedStep::Send(ResolvedFrame::Ping("cGluZw==".into())),
        wait_regex("pinged", 10_000),
        close(),
    ];
    let exchange = run(&target(&server.url("/"), steps), &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    let recorded = &server.sessions()[0].messages;
    assert!(
        recorded.contains(&Message::Pong(b"srv".to_vec().into())),
        "{recorded:?}"
    );
    assert_eq!(entries(&exchange, Direction::Received, "ping").len(), 1);
    assert_eq!(entries(&exchange, Direction::Sent, "pong").len(), 1);
    let pongs = entries(&exchange, Direction::Received, "pong");
    assert_eq!(pongs.len(), 1, "{:?}", exchange.timeline.to_vec());
    assert_eq!(pongs[0].payload, "cGluZw==");
    assert!(pongs[0].rtt_ms.is_some(), "{pongs:?}");
}

#[tokio::test]
async fn a_server_close_is_recorded_with_code_and_reason() {
    let server = RealtimeWsServer::start(
        WsBehavior::default().on_connect(vec![WsAction::Close(1008, "policy".into())]),
    )
    .await;
    let exchange = run(&target(&server.url("/"), vec![]), &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    let end = exchange.end.as_ref().unwrap();
    assert_eq!(end.reason, "closed by server: 1008 policy");
    assert_eq!(exchange.close_code(), Some(1008));
    assert_eq!(end.close_reason.as_deref(), Some("policy"));
    let closes = entries(&exchange, Direction::Received, "close");
    assert_eq!(closes[0].payload, "1008 policy");
}

#[tokio::test]
async fn a_close_step_sends_its_code_and_reason() {
    let server = RealtimeWsServer::start(WsBehavior::default()).await;
    let close = ResolvedStep::Close(CloseStep {
        code: Some(4000),
        reason: Some("bye".into()),
    });
    let exchange = run(
        &target(&server.url("/"), vec![close]),
        &SendOptions::default(),
    )
    .await;
    assert_eq!(exchange.error, None);
    assert_eq!(
        exchange.end.as_ref().unwrap().reason,
        "closed by client: 4000 bye"
    );
    let recorded = &server.sessions()[0].messages;
    let Some(Message::Close(Some(frame))) = recorded.last() else {
        panic!("a close frame: {recorded:?}");
    };
    assert_eq!(
        (u16::from(frame.code), frame.reason.as_str()),
        (4000, "bye")
    );
    // The server's answer completed the handshake (not the 2000 ms wait).
    assert_eq!(entries(&exchange, Direction::Received, "close").len(), 1);
}

#[tokio::test]
async fn sending_after_the_server_closed_fails_with_not_connected() {
    let server = RealtimeWsServer::start(
        WsBehavior::default().on_connect(vec![WsAction::Close(1000, String::new())]),
    )
    .await;
    // A generous pause: the server's close must be read before the send.
    let steps = vec![sleep(2000), text("late")];
    let exchange = run(&target(&server.url("/"), steps), &SendOptions::default()).await;
    assert_eq!(
        exchange.error.as_ref().map(ToString::to_string).as_deref(),
        Some("ws: not connected")
    );
}

#[tokio::test]
async fn wait_for_matches_a_json_reply() {
    let server = RealtimeWsServer::start(WsBehavior::default().reply(|message| match message {
        Message::Text(t) if t.as_str().contains("ping") => vec![
            WsAction::Text(r#"{"id":2,"op":"other"}"#.into()),
            WsAction::Pause(Duration::from_millis(50)),
            WsAction::Text(r#"{"id":1,"op":"pong"}"#.into()),
        ],
        _ => Vec::new(),
    }))
    .await;
    let wait = ResolvedStep::WaitFor(Box::new(WaitFor {
        json: Some(
            Checked::new(JsonAssertion {
                path: "$.id".into(),
                equals: Some(json!(1)),
                ..empty_json()
            })
            .unwrap(),
        ),
        regex: None,
        timeout_ms: Some(5000),
    }));
    let steps = vec![
        ResolvedStep::Send(ResolvedFrame::Json(json!({"id": 1, "op": "ping"}))),
        wait,
        text("after"),
        close(),
    ];
    let exchange = run(&target(&server.url("/"), steps), &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    let received: Vec<String> = entries(&exchange, Direction::Received, "json")
        .into_iter()
        .map(|e| e.payload)
        .collect();
    assert_eq!(
        received,
        [r#"{"id":2,"op":"other"}"#, r#"{"id":1,"op":"pong"}"#]
    );
    assert_eq!(
        server.sessions()[0].texts().last().map(String::as_str),
        Some("after")
    );
}

fn empty_json() -> JsonAssertion {
    JsonAssertion {
        path: String::new(),
        equals: None,
        not_equals: None,
        contains: None,
        matches: None,
        gt: None,
        gte: None,
        lt: None,
        lte: None,
        exists: None,
        length: None,
        value_type: None,
    }
}

#[tokio::test]
async fn a_wait_for_without_a_match_times_out() {
    let server = RealtimeWsServer::start(
        WsBehavior::default().on_connect(vec![WsAction::Text("ping".into())]),
    )
    .await;
    let exchange = run(
        &target(
            &server.url("/"),
            vec![wait_regex("pong", 300), text("never")],
        ),
        &SendOptions::default(),
    )
    .await;
    assert_eq!(
        exchange.error.as_ref().map(ToString::to_string).as_deref(),
        Some("ws: wait_for timed out after 300 ms (regex pong)")
    );
    assert!(
        server.sessions()[0].texts().is_empty(),
        "later steps do not run"
    );
    assert_eq!(
        entries(&exchange, Direction::Received, "text")[0].payload,
        "ping"
    );
    assert_eq!(exchange.close_code(), Some(1000));
}

#[tokio::test]
async fn messages_received_outside_a_wait_are_recorded() {
    let server = RealtimeWsServer::start(
        WsBehavior::default().on_connect(vec![WsAction::Text("hello".into())]),
    )
    .await;
    let steps = vec![wait_regex("hello", 5000), sleep(100), close()];
    let exchange = run(&target(&server.url("/"), steps), &SendOptions::default()).await;
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.received, 1);
}

#[tokio::test]
async fn an_endless_feed_stops_at_max_messages() {
    let server =
        RealtimeWsServer::start(WsBehavior::default().on_connect(vec![WsAction::Forever {
            text: r#"{"n":{n}}"#.into(),
            every: Duration::from_millis(1),
        }]))
        .await;
    let session = target(&server.url("/"), vec![]);
    let bounds = Bounds::of(&session.limits);
    let options = SessionOptions::headless(bounds, CancellationToken::new());
    let exchange = run_ws(&session, &SendOptions::default(), options).await;
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.received, 50);
    assert_eq!(exchange.close_code(), Some(1000));
    assert_eq!(exchange.end.unwrap().reason, "max_messages reached (50)");
}

#[tokio::test]
async fn an_oversized_frame_closes_with_1009() {
    let big = vec![b'x'; 2 * 1024 * 1024];
    let server =
        RealtimeWsServer::start(WsBehavior::default().on_connect(vec![WsAction::Binary(big)]))
            .await;
    let mut session = target(&server.url("/"), vec![sleep(5000)]);
    session.limits.max_message_bytes = Some(1_048_576);
    let exchange = run(&session, &SendOptions::default()).await;
    assert_eq!(exchange.close_code(), Some(1009));
    let notes = entries(&exchange, Direction::System, "close");
    assert_eq!(notes[0].payload, "message too large: 2097152 bytes");
    let received = server_close_code(&server).await;
    assert_eq!(received, Some(1009), "the server got the 1009 close frame");
}

/// After a 1009 close the client keeps reading until the server ends the
/// connection, instead of dropping a socket with unread bytes: that resets
/// the connection, and on Windows the reset made the server lose the close
/// frame. The server here sends far more than socket buffers hold, so its
/// send completes only when the client reads it all.
#[tokio::test]
async fn after_a_1009_close_the_client_reads_until_the_server_closes() {
    use futures_util::{SinkExt, StreamExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let (mut sink, mut stream) = socket.split();
        let writer = tokio::spawn(async move {
            sink.send(Message::binary(vec![b'x'; 16 * 1024 * 1024]))
                .await
        });
        let close = loop {
            match stream.next().await {
                Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
                Some(Ok(_)) => {}
                other => panic!("no close frame: {other:?}"),
            }
        };
        let written = writer.await.unwrap();
        (close, written.is_ok())
    });
    let mut session = target(&url, vec![sleep(5000)]);
    session.limits.max_message_bytes = Some(1_048_576);
    let exchange = run(&session, &SendOptions::default()).await;
    assert_eq!(exchange.close_code(), Some(1009));
    let (close, written) = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the server finished")
        .unwrap();
    assert_eq!(close, Some(1009));
    assert!(
        written,
        "the client read the rest of the message, not reset"
    );
}

/// The code of the first close frame the server received, waiting up to
/// 10 s for it.
async fn server_close_code(server: &RealtimeWsServer) -> Option<u16> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let code = server
            .sessions()
            .iter()
            .flat_map(|s| s.messages.clone())
            .find_map(|m| match m {
                Message::Close(frame) => Some(frame.map(|f| u16::from(f.code))),
                _ => None,
            });
        if code.is_some() || tokio::time::Instant::now() > deadline {
            return code.flatten();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_live_feed_is_redacted() {
    let canary = "s3cr3t-ws-feed-canary";
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let mut session = target(
        &server.url("/"),
        vec![
            text(&format!("token={canary}")),
            wait_regex("token", 5000),
            close(),
        ],
    );
    session.http.secrets = vec![Secret::new("api_token", canary.to_owned())];
    let (feed, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let mut options = headless(1_000);
    options.feed = Some(feed);
    let exchange = run_ws(&session, &SendOptions::default(), options).await;
    assert_eq!(exchange.error, None);
    let mut shown = Vec::new();
    while let Ok(entry) = entries.try_recv() {
        shown.push(serde_json::to_string(&entry).unwrap());
    }
    let shown = shown.join("\n");
    assert!(!shown.contains(canary), "{shown}");
    assert!(shown.contains("token=«redacted:api_token»"), "{shown}");
}

#[tokio::test]
async fn an_interactive_session_runs_commands_until_closed() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let (commands, rx) = rustman_realtime::command_channel();
    let (feed, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let options = SessionOptions {
        bounds: None,
        cancel: CancellationToken::new(),
        feed: Some(feed),
        commands: Some(rx),
    };
    let session = target(&server.url("/"), vec![]);
    let task =
        tokio::spawn(async move { run_ws(&session, &SendOptions::default(), options).await });
    commands
        .send(rustman_realtime::Command::Send(ResolvedFrame::Text(
            "a".into(),
        )))
        .await
        .unwrap();
    loop {
        let entry = entries.recv().await.expect("an entry");
        if entry.dir == Direction::Received {
            assert_eq!(entry.payload, "a");
            break;
        }
    }
    commands
        .send(rustman_realtime::Command::Close(CloseStep::default()))
        .await
        .unwrap();
    let exchange = task.await.unwrap();
    assert_eq!(exchange.error, None);
    assert_eq!(exchange.end.unwrap().reason, "closed by client: 1000");
}
