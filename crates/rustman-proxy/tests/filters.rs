//! Filters and limits (traffic-recorder spec: Capture filters, Capture
//! limits): filtered exchanges are still proxied, and oversized bodies
//! reach the client in full while the capture keeps the first `max_body`
//! bytes.

mod common;

use std::time::Duration;

use rustman_proxy::{FilterRules, ProxyConfig};
use rustman_test_support::server::TestServer;
use tokio::net::TcpStream;

use common::{get, send_http1, start};

/// Give a filtered exchange time to be (wrongly) recorded.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn static_assets_are_proxied_but_not_recorded() {
    let server = TestServer::http().await;
    let proxy = start(ProxyConfig::default(), None).await;
    let png = send_http1(
        TcpStream::connect(proxy.addr).await.unwrap(),
        get(&server.url("/png")),
    )
    .await;
    assert_eq!(png.status(), 200);
    assert_eq!(png.headers()["content-type"], "image/png");
    assert!(!png.body().is_empty(), "the asset reaches the client");
    let api = send_http1(
        TcpStream::connect(proxy.addr).await.unwrap(),
        get(&server.url("/echo")),
    )
    .await;
    assert_eq!(api.status(), 200);

    let captures = proxy.captures(1).await;
    settle().await;
    let urls: Vec<String> = proxy
        .store
        .snapshot()
        .iter()
        .filter_map(|c| c.exchange().map(|e| e.url.clone()))
        .collect();
    assert_eq!(urls, [server.url("/echo")], "{captures:?}");
}

#[tokio::test]
async fn method_and_host_filters_still_proxy_everything() {
    let server = TestServer::http().await;
    let config = ProxyConfig {
        filter: FilterRules {
            methods: vec!["POST".into()],
            exclude_hosts: vec!["localhost".into()],
            ..FilterRules::default()
        },
        ..ProxyConfig::default()
    };
    let proxy = start(config, None).await;
    let skipped = send_http1(
        TcpStream::connect(proxy.addr).await.unwrap(),
        get(&server.url("/echo")),
    )
    .await;
    assert_eq!(skipped.status(), 200, "a GET is proxied");
    let excluded = server.url("/echo");
    let kept_url = format!("http://127.0.0.1:{}/echo", server.port());
    let request = http::Request::post(&excluded)
        .body(http_body_util::Full::default())
        .unwrap();
    let response = send_http1(TcpStream::connect(proxy.addr).await.unwrap(), request).await;
    assert_eq!(response.status(), 200, "an excluded host is proxied");
    let request = http::Request::post(&kept_url)
        .body(http_body_util::Full::default())
        .unwrap();
    send_http1(TcpStream::connect(proxy.addr).await.unwrap(), request).await;

    let captures = proxy.captures(1).await;
    settle().await;
    let kept: Vec<(String, String)> = proxy
        .store
        .snapshot()
        .iter()
        .filter_map(|c| c.exchange().map(|e| (e.method.clone(), e.url.clone())))
        .collect();
    assert_eq!(kept, [("POST".to_owned(), kept_url)], "{captures:?}");
}

#[tokio::test]
async fn a_25_mib_response_is_delivered_in_full_and_stored_truncated() {
    const SIZE: usize = 25 * 1024 * 1024;
    let server = TestServer::http().await;
    let proxy = start(ProxyConfig::default(), None).await;
    let url = server.url(&format!("/bytes/{SIZE}"));
    let response = send_http1(TcpStream::connect(proxy.addr).await.unwrap(), get(&url)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body().len(), SIZE, "the client gets every byte");

    let captures = proxy.captures(1).await;
    let body = &captures[0].exchange().unwrap().response_body;
    assert_eq!(body.bytes.len(), 10 * 1024 * 1024);
    assert_eq!(body.total, SIZE as u64);
    assert!(body.truncated);
    assert!(captures[0].to_string().contains(" truncated"));
}
