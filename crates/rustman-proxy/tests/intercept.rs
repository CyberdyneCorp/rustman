//! HTTPS interception (traffic-recorder spec: Interception scope,
//! Certificate pinning failures): in-scope hosts are decrypted with a leaf
//! from the local CA over HTTP/1.1 or HTTP/2, out-of-scope hosts stay
//! tunnels, and clients that refuse the CA leave a `TLS REJECTED` entry.

mod common;

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use rustman_proxy::ca::LocalCa;
use rustman_proxy::{ProxyConfig, Recorded, TLS_REJECTED_HINT};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{TestServer, Tls};

use common::{Running, client_tls, connect, handshake, send_h2, send_http1, start, trusting};

async fn intercepting(certs: &TestCerts, ca: &LocalCa) -> Running {
    let config = ProxyConfig {
        intercept: vec!["localhost".into()],
        upstream_tls: trusting(&certs.ca_pem),
        ..ProxyConfig::default()
    };
    start(config, Some(ca)).await
}

#[tokio::test]
async fn an_included_host_is_fully_captured() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let proxy = intercepting(&certs, &ca).await;
    let authority = format!("localhost:{}", server.port());
    let tcp = connect(&proxy, &authority).await;
    let tls = handshake(tcp, &client_tls(ca.cert_pem(), &[b"http/1.1"]), "localhost")
        .await
        .expect("the client trusts the local CA");
    let request = Request::post("/echo")
        .header("host", "elsewhere.test")
        .header("authorization", "Bearer abcdef123456")
        .body(Full::new(Bytes::from_static(b"hello")))
        .unwrap();
    let response = send_http1(tls, request).await;
    assert_eq!(response.status(), 200);
    let echoed = String::from_utf8(response.body().to_vec()).unwrap();
    assert!(
        echoed.contains(&format!("\"localhost:{}\"", server.port())),
        "the request goes to the CONNECT host, not the Host header: {echoed}"
    );

    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().expect("an exchange");
    assert!(exchange.intercepted);
    assert_eq!(exchange.url, format!("https://{authority}/echo"));
    assert_eq!(exchange.status, Some(200));
    assert_eq!(exchange.version, "HTTP/1.1");
    assert_eq!(exchange.request_body.bytes, b"hello");
    assert_eq!(exchange.response_body.bytes, response.body().to_vec());
    assert!(
        exchange
            .request_headers
            .contains(&("authorization".into(), "Bearer abcdef123456".into()))
    );
}

#[tokio::test]
async fn a_host_not_included_stays_a_tunnel() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::Http1Only).await;
    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let proxy = intercepting(&certs, &ca).await;
    // `127.0.0.1` is not in the scope (`localhost` is).
    let tcp = connect(&proxy, &format!("127.0.0.1:{}", server.port())).await;
    let tls = handshake(tcp, &client_tls(&certs.ca_pem, &[b"http/1.1"]), "localhost")
        .await
        .expect("the real server's certificate reaches the client");
    let response = send_http1(tls, common::get("/echo")).await;
    assert_eq!(response.status(), 200);
    drop(response);

    let captures = proxy.captures(1).await;
    assert!(
        matches!(&captures[0].recorded, Recorded::Tunnel(t) if t.host == "127.0.0.1"),
        "{captures:?}"
    );
}

#[tokio::test]
async fn a_client_rejecting_the_ca_leaves_a_tls_rejected_entry() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let proxy = intercepting(&certs, &ca).await;
    let tcp = connect(&proxy, &format!("localhost:{}", server.port())).await;
    // The client trusts the test CA only, not the local one.
    let refused = handshake(tcp, &client_tls(&certs.ca_pem, &[b"http/1.1"]), "localhost").await;
    assert!(refused.is_err());

    let captures = proxy.captures(1).await;
    assert_eq!(
        captures[0].recorded,
        Recorded::TlsRejected {
            host: "localhost".into(),
            port: server.port()
        }
    );
    assert_eq!(
        captures[0].to_string(),
        format!("TLS REJECTED localhost: {TLS_REJECTED_HINT}")
    );
}

#[tokio::test]
async fn http2_is_negotiated_by_alpn_and_captured() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let proxy = intercepting(&certs, &ca).await;
    let authority = format!("localhost:{}", server.port());
    let tcp = connect(&proxy, &authority).await;
    let tls = handshake(tcp, &client_tls(ca.cert_pem(), &[b"h2"]), "localhost")
        .await
        .unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let url = format!("https://{authority}/echo?q=1");
    let response = send_h2(tls, common::get(&url)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.version(), http::Version::HTTP_2);

    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert_eq!(exchange.version, "HTTP/2");
    assert_eq!(exchange.url, url);
    assert_eq!(exchange.status, Some(200));
    assert_eq!(exchange.response_body.bytes, response.body().to_vec());
}

#[tokio::test]
async fn a_request_head_trickled_after_the_handshake_is_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let ca = LocalCa::generate("Rustman Test CA").unwrap();
    let config = ProxyConfig {
        intercept: vec!["localhost".into()],
        client_timeout: std::time::Duration::from_millis(300),
        ..ProxyConfig::default()
    };
    let proxy = start(config, Some(&ca)).await;
    let tcp = connect(&proxy, "localhost:443").await;
    let mut tls = handshake(tcp, &client_tls(ca.cert_pem(), &[b"http/1.1"]), "localhost")
        .await
        .expect("the client trusts the local CA");
    // Part of a head, never finished.
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tls.read_to_end(&mut rest),
    )
    .await;
    assert!(
        closed.is_ok(),
        "the proxy keeps a trickling connection open"
    );
    assert!(proxy.store.is_empty());
}
