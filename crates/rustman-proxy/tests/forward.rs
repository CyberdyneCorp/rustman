//! The forward proxy (traffic-recorder spec: Forward proxy command,
//! Loopback by default, Interception scope): plain HTTP forwarding byte for
//! byte with a capture, opaque `CONNECT` tunnels with byte counts, and the
//! bind errors.

mod common;

use std::sync::atomic::Ordering;

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use rustman_proxy::{BindError, Proxy, ProxyConfig, ProxyError, Recorded};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{TestServer, Tls};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use common::{Counting, client_tls, connect, get, handshake, send_http1, start};

#[tokio::test]
async fn plain_http_passes_through_byte_for_byte_and_is_captured() {
    let server = TestServer::http().await;
    let proxy = start(ProxyConfig::default(), None).await;
    let url = server.url("/compressed/gzip");
    let direct = send_http1(TcpStream::connect(server.addr()).await.unwrap(), get(&url)).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let proxied = send_http1(tcp, get(&url)).await;

    assert_eq!(proxied.status(), 200);
    assert_eq!(proxied.headers()["content-encoding"], "gzip");
    assert_eq!(proxied.body(), direct.body(), "the body is not decoded");
    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().expect("an exchange");
    assert_eq!(exchange.method, "GET");
    assert_eq!(exchange.url, url);
    assert_eq!(exchange.status, Some(200));
    assert!(!exchange.intercepted);
    assert_eq!(exchange.response_body.bytes, direct.body().to_vec());
    assert_eq!(exchange.response_body.total, direct.body().len() as u64);
}

#[tokio::test]
async fn request_bodies_and_headers_are_captured() {
    let server = TestServer::http().await;
    let proxy = start(ProxyConfig::default(), None).await;
    let request = Request::post(server.url("/echo"))
        .header("x-trace", "t-1")
        .header("proxy-connection", "keep-alive")
        .body(Full::new(Bytes::from_static(b"{\"a\":1}")))
        .unwrap();
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, request).await;
    let echoed = String::from_utf8(response.body().to_vec()).unwrap();
    assert!(echoed.contains("t-1"), "{echoed}");
    assert!(
        !echoed.contains("proxy-connection"),
        "hop-by-hop dropped: {echoed}"
    );

    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert_eq!(exchange.request_body.bytes, b"{\"a\":1}");
    assert!(
        exchange
            .request_headers
            .contains(&("x-trace".into(), "t-1".into()))
    );
}

#[tokio::test]
async fn an_unreachable_upstream_answers_502_and_is_captured() {
    let (port, _reserved) = closed_port();
    let proxy = start(ProxyConfig::default(), None).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, get(&format!("http://127.0.0.1:{port}/x"))).await;
    assert_eq!(response.status(), 502);
    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert_eq!(exchange.status, None);
    assert!(
        exchange
            .error
            .as_deref()
            .unwrap()
            .starts_with("upstream error")
    );
}

/// A port that refuses connections for as long as the socket is held: it
/// is bound but never listens, so no other test can take it.
fn closed_port() -> (u16, tokio::net::TcpSocket) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    (socket.local_addr().unwrap().port(), socket)
}

#[tokio::test]
async fn credentials_in_a_url_are_refused_and_never_recorded() {
    let server = TestServer::http().await;
    let proxy = start(ProxyConfig::default(), None).await;
    let url = server
        .url("/echo")
        .replace("http://", "http://alice:s3cr3tpass@");
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, get(&url)).await;
    assert_eq!(response.status(), 400);
    let body = String::from_utf8_lossy(response.body());
    assert!(!body.contains("s3cr3tpass"), "{body}");
    assert!(body.contains("Authorization header"), "{body}");
    assert!(proxy.store.is_empty());
}

#[tokio::test]
async fn origin_form_requests_are_refused() {
    let proxy = start(ProxyConfig::default(), None).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, get("/v1/users")).await;
    assert_eq!(response.status(), 400);
    assert!(proxy.store.is_empty());
}

#[tokio::test]
async fn connect_tunnels_are_relayed_without_decryption() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::Http1Only).await;
    let config = ProxyConfig {
        intercept: vec!["*.example.com".into()],
        ..ProxyConfig::default()
    };
    let ca = rustman_proxy::ca::LocalCa::generate("Rustman Test").unwrap();
    let proxy = start(config, Some(&ca)).await;
    let authority = format!("localhost:{}", server.port());
    let tcp = Counting::new(connect(&proxy, &authority).await);
    let (written, read) = (tcp.written.clone(), tcp.read.clone());

    let mut tls = handshake(tcp, &client_tls(&certs.ca_pem, &[b"http/1.1"]), "localhost")
        .await
        .expect("the client talks to the real server through the tunnel");
    tls.write_all(b"GET /echo HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = tls.read_to_end(&mut response).await;
    drop(tls);
    assert!(response.starts_with(b"HTTP/1.1 200"));

    let captures = proxy.captures(1).await;
    let Recorded::Tunnel(tunnel) = &captures[0].recorded else {
        panic!("a tunnel, got {:?}", captures[0].recorded);
    };
    assert_eq!(
        (tunnel.host.as_str(), tunnel.port),
        ("localhost", server.port())
    );
    assert_eq!(tunnel.bytes_up, written.load(Ordering::SeqCst));
    assert_eq!(tunnel.bytes_down, read.load(Ordering::SeqCst));
    assert!(tunnel.bytes_up > 0 && tunnel.bytes_down > 0);
    assert!(
        captures.iter().all(|c| c.exchange().is_none()),
        "nothing decrypted"
    );
    assert_eq!(
        captures[0].to_string(),
        format!(
            "TUNNEL localhost:{} {} B up, {} B down",
            server.port(),
            tunnel.bytes_up,
            tunnel.bytes_down
        )
    );
}

#[tokio::test]
async fn a_busy_port_is_a_bind_error() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let config = ProxyConfig {
        port,
        ..ProxyConfig::default()
    };
    let store = rustman_proxy::CaptureStore::new(10, 1024);
    let Err(err) = Proxy::bind(&config, store, None).await else {
        panic!("bound a busy port");
    };
    assert_eq!(
        err.to_string(),
        format!("record: cannot bind 127.0.0.1:{port}: address in use")
    );
    assert!(matches!(err, ProxyError::Bind(BindError::Bind { .. })));
}

#[tokio::test]
async fn interception_needs_a_ca() {
    let config = ProxyConfig {
        port: 0,
        intercept: vec!["localhost".into()],
        ..ProxyConfig::default()
    };
    let store = rustman_proxy::CaptureStore::new(10, 1024);
    let Err(err) = Proxy::bind(&config, store, None).await else {
        panic!("intercepting without a CA");
    };
    assert!(matches!(err, ProxyError::NoCa), "{err}");
}

#[tokio::test]
async fn connections_beyond_the_limit_wait_for_a_free_slot() {
    let server = TestServer::http().await;
    let config = ProxyConfig {
        max_connections: 1,
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let url = server.url("/echo");
    // A kept-alive connection that was answered holds the only slot.
    let (mut sender, connection) = hyper::client::conn::http1::handshake(
        hyper_util::rt::TokioIo::new(TcpStream::connect(proxy.addr).await.unwrap()),
    )
    .await
    .unwrap();
    let idle = tokio::spawn(connection);
    let first = sender.send_request(get(&url)).await.unwrap();
    assert_eq!(first.status(), 200);
    let waiting = tokio::spawn(async move {
        send_http1(TcpStream::connect(proxy.addr).await.unwrap(), get(&url)).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!waiting.is_finished(), "a second connection is not served");
    drop(first);
    drop(sender);
    let _ = idle.await;
    let response = tokio::time::timeout(std::time::Duration::from_secs(20), waiting)
        .await
        .expect("served once the first connection closes")
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn an_intercepted_tunnel_without_a_handshake_is_closed() {
    let config = ProxyConfig {
        intercept: vec!["localhost".into()],
        client_timeout: std::time::Duration::from_millis(200),
        ..ProxyConfig::default()
    };
    let ca = rustman_proxy::ca::LocalCa::generate("Rustman Test").unwrap();
    let proxy = start(config, Some(&ca)).await;
    let mut tcp = connect(&proxy, "localhost:443").await;
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(20), tcp.read(&mut byte))
        .await
        .expect("the proxy closes a silent tunnel");
    assert_eq!(read.unwrap_or(0), 0);
}

#[tokio::test]
async fn an_upstream_on_the_proxy_itself_is_refused() {
    let (port, reserved) = closed_port();
    // Free it just before the proxy binds it.
    drop(reserved);
    let config = ProxyConfig {
        port,
        upstream: Some(format!("http://localhost:{port}")),
        ..ProxyConfig::default()
    };
    let store = rustman_proxy::CaptureStore::new(10, usize::MAX);
    let Err(err) = Proxy::bind(&config, store, None).await else {
        panic!("the proxy started");
    };
    assert!(matches!(err, ProxyError::UpstreamLoop(_)), "{err}");
    assert_eq!(
        err.to_string(),
        format!(
            "record: --upstream \"http://localhost:{port}\" is this proxy's own address; requests would loop"
        )
    );
}
