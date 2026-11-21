//! Resource limits of the proxy (traffic-recorder spec: Capture limits,
//! Connection timeouts): the copies of bodies in flight share the
//! `max_buffer_bytes` budget, a slow upstream response head gets `504`, and
//! idle tunnels, intercepted connections and plain connections with a
//! stalled request body are closed.

mod common;

use std::time::Duration;

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustman_proxy::ca::LocalCa;
use rustman_proxy::{ProxyConfig, Recorded};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{TestServer, Tls};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use common::{client_tls, connect, get, handshake, send_http1, start, trusting};

const SHORT: Duration = Duration::from_millis(300);
const PATIENCE: Duration = Duration::from_secs(10);

#[tokio::test]
async fn bodies_past_the_in_flight_budget_are_stored_truncated_and_still_proxied() {
    let server = TestServer::http().await;
    let config = ProxyConfig {
        max_buffer_bytes: 40,
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let payload = "x".repeat(100);
    let request = Request::post(server.url("/echo"))
        .body(Full::new(Bytes::from(payload.clone())))
        .unwrap();
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, request).await;
    assert_eq!(response.status(), 200);
    let echoed = String::from_utf8(response.body().to_vec()).unwrap();
    assert!(echoed.contains(&payload), "the traffic flows in full");

    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    let request_body = &exchange.request_body;
    assert_eq!(request_body.total, 100);
    assert!(request_body.truncated, "{request_body:?}");
    assert!(request_body.bytes.len() <= 40);
    let response_body = &exchange.response_body;
    assert_eq!(response_body.total, response.body().len() as u64);
    assert!(response_body.truncated, "the budget is held by the request");
    assert!(response_body.bytes.is_empty());
}

#[tokio::test]
async fn an_upstream_without_a_response_head_gets_504() {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = silent.local_addr().unwrap();
    let held = tokio::spawn(async move {
        let (mut tcp, _) = silent.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        while tcp.read(&mut buf).await.is_ok_and(|n| n > 0) {}
    });
    let config = ProxyConfig {
        response_timeout: SHORT,
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let send = send_http1(tcp, get(&format!("http://{addr}/slow")));
    let response = tokio::time::timeout(PATIENCE, send)
        .await
        .expect("the proxy waits for the head forever");
    assert_eq!(response.status(), 504);
    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert_eq!(exchange.status, None);
    let error = exchange.error.as_deref().unwrap_or_default();
    assert!(error.contains("no response within 0.3 s"), "{error}");
    held.abort();
}

#[tokio::test]
async fn an_idle_opaque_tunnel_is_closed_and_recorded() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut tcp, _) = echo.accept().await.unwrap();
        let mut buf = [0u8; 64];
        while let Ok(n @ 1..) = tcp.read(&mut buf).await {
            tcp.write_all(&buf[..n]).await.unwrap();
        }
    });
    let config = ProxyConfig {
        idle_timeout: SHORT,
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let mut tcp = connect(&proxy, &format!("127.0.0.1:{port}")).await;
    tcp.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    tcp.read_exact(&mut buf).await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(PATIENCE, tcp.read_to_end(&mut rest))
        .await
        .expect("the idle tunnel stays open")
        .unwrap();
    let captures = proxy.captures(1).await;
    let Recorded::Tunnel(tunnel) = &captures[0].recorded else {
        panic!("{captures:?}");
    };
    assert_eq!((tunnel.bytes_up, tunnel.bytes_down), (4, 4));
    let error = tunnel.error.as_deref().unwrap_or_default();
    assert!(error.starts_with("idle timeout"), "{error}");
}

#[tokio::test]
async fn an_idle_intercepted_http2_connection_is_closed() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let config = ProxyConfig {
        intercept: vec!["localhost".into()],
        upstream_tls: trusting(&certs.ca_pem),
        idle_timeout: SHORT,
        ..ProxyConfig::default()
    };
    let proxy = start(config, Some(&ca)).await;
    let authority = format!("localhost:{}", server.port());
    let tcp = connect(&proxy, &authority).await;
    let tls = handshake(tcp, &client_tls(ca.cert_pem(), &[b"h2"]), "localhost")
        .await
        .unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    let conn = tokio::spawn(conn);
    let response = sender
        .send_request(get(&format!("https://{authority}/echo")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    drop(response);
    // The sender is kept: only the proxy can end the connection.
    tokio::time::timeout(PATIENCE, conn)
        .await
        .expect("the idle intercepted connection stays open")
        .unwrap()
        .ok();
    drop(sender);
}

/// An upstream that reads whatever it gets and never answers.
async fn silent_upstream() -> std::net::SocketAddr {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = silent.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = silent.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                while tcp.read(&mut buf).await.is_ok_and(|n| n > 0) {}
            });
        }
    });
    addr
}

/// Send `head` and the start of its body through a proxy that serves one
/// connection at once, stall, and expect the proxy to close the connection
/// and serve the next client.
async fn assert_stalled_body_is_closed(head_and_partial_body: &str) {
    let upstream = silent_upstream().await;
    let server = TestServer::http().await;
    let config = ProxyConfig {
        idle_timeout: SHORT,
        max_connections: 1,
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let mut tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let request = head_and_partial_body.replace("{upstream}", &upstream.to_string());
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(PATIENCE, tcp.read_to_end(&mut rest)).await;
    assert!(closed.is_ok(), "the stalled connection stays open");
    let next = TcpStream::connect(proxy.addr).await.unwrap();
    let response = tokio::time::timeout(PATIENCE, send_http1(next, get(&server.url("/echo"))))
        .await
        .expect("the stalled connection still holds the only connection slot");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_plain_connection_with_a_stalled_content_length_body_is_closed() {
    assert_stalled_body_is_closed(
        "POST http://{upstream}/upload HTTP/1.1\r\nHost: {upstream}\r\n\
         Content-Length: 100000\r\n\r\npartial",
    )
    .await;
}

#[tokio::test]
async fn a_plain_connection_with_a_stalled_chunked_body_is_closed() {
    assert_stalled_body_is_closed(
        "POST http://{upstream}/upload HTTP/1.1\r\nHost: {upstream}\r\n\
         Transfer-Encoding: chunked\r\n\r\n7\r\npartial\r\n",
    )
    .await;
}
