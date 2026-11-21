//! Reverse-proxy mode (traffic-recorder spec: Reverse-proxy mode):
//! origin-form requests go to the fixed upstream with `Host` rewritten,
//! are recorded like forward mode, and HTTPS upstreams are verified with
//! the `http-requests` TLS rules.

mod common;

use rustman_proxy::{Proxy, ProxyConfig, ProxyError, Recorded};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{TestServer, Tls};
use tokio::net::TcpStream;

use common::{get, send_http1, start};

fn reverse(upstream: &str) -> ProxyConfig {
    ProxyConfig {
        upstream: Some(upstream.to_owned()),
        ..ProxyConfig::default()
    }
}

#[tokio::test]
async fn origin_form_requests_reach_the_upstream_and_are_captured() {
    let server = TestServer::http().await;
    let upstream = format!("http://127.0.0.1:{}", server.port());
    let proxy = start(reverse(&upstream), None).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let request = http::Request::get("/echo?v=1")
        .header("host", proxy.addr.to_string())
        .body(http_body_util::Full::default())
        .unwrap();
    let response = send_http1(tcp, request).await;
    assert_eq!(response.status(), 200);
    let echoed = String::from_utf8(response.body().to_vec()).unwrap();
    assert!(
        echoed.contains(&format!("127.0.0.1:{}", server.port())),
        "Host is rewritten to the upstream: {echoed}"
    );

    let captures = proxy.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert_eq!(exchange.url, format!("{upstream}/echo?v=1"));
    assert_eq!(exchange.status, Some(200));
}

#[tokio::test]
async fn absolute_form_requests_cannot_steer_away_from_the_upstream() {
    let server = TestServer::http().await;
    let upstream = format!("http://127.0.0.1:{}", server.port());
    let proxy = start(reverse(&upstream), None).await;
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let response = send_http1(tcp, get("http://elsewhere.invalid/echo")).await;
    assert_eq!(response.status(), 200);
    let captures = proxy.captures(1).await;
    assert_eq!(
        captures[0].exchange().unwrap().url,
        format!("{upstream}/echo")
    );
}

#[tokio::test]
async fn a_self_signed_upstream_fails_unless_verification_is_disabled() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::Http1Only).await;
    let upstream = format!("https://localhost:{}", server.port());

    let verified = start(reverse(&upstream), None).await;
    let tcp = TcpStream::connect(verified.addr).await.unwrap();
    let request = http::Request::get("/v1/users")
        .header("host", "x")
        .body(http_body_util::Full::default())
        .unwrap();
    let response = send_http1(tcp, request).await;
    assert_eq!(response.status(), 502);
    let captures = verified.captures(1).await;
    let exchange = captures[0].exchange().unwrap();
    assert!(exchange.error.is_some(), "{exchange:?}");

    let mut config = reverse(&upstream);
    config.upstream_tls.verify = false;
    let insecure = start(config, None).await;
    let tcp = TcpStream::connect(insecure.addr).await.unwrap();
    let response = send_http1(tcp, get("/echo")).await;
    assert_eq!(response.status(), 200);
    let captures = insecure.captures(1).await;
    assert!(
        matches!(&captures[0].recorded, Recorded::Exchange(e) if e.url == format!("{upstream}/echo"))
    );
}

#[tokio::test]
async fn an_upstream_must_be_an_origin() {
    for bad in [
        "ftp://a.test",
        "https://a.test/v1",
        "not a url",
        "https://u:p@a.test",
    ] {
        let config = ProxyConfig {
            port: 0,
            ..reverse(bad)
        };
        let store = rustman_proxy::CaptureStore::new(10, 1024);
        let err = Proxy::bind(&config, store, None).await.err().expect(bad);
        assert!(matches!(err, ProxyError::Upstream(_)), "{bad}: {err}");
    }
}
