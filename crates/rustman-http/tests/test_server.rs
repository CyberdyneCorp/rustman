//! Self-test of the in-process test server fixtures (task 4.1).

mod support;

use std::time::{Duration, Instant};

use async_compression::tokio::bufread::{BrotliDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder};
use http::{StatusCode, Version};
use support::raw::{Mode, get, try_get};
use support::routes::{COMPRESSED_TEXT, PNG};
use support::{TestCerts, TestServer, Tls};
use tokio::io::AsyncReadExt;

fn certs() -> TestCerts {
    TestCerts::generate()
}

#[tokio::test]
async fn cleartext_serves_http1_and_h2c() {
    let certs = certs();
    let server = TestServer::http().await;
    let h1 = get(&server, "/status/204", Mode::Http1, &certs).await;
    assert_eq!(
        (h1.version, h1.status),
        (Version::HTTP_11, StatusCode::NO_CONTENT)
    );
    let h2 = get(&server, "/status/201", Mode::H2c, &certs).await;
    assert_eq!(
        (h2.version, h2.status),
        (Version::HTTP_2, StatusCode::CREATED)
    );
}

#[tokio::test]
async fn tls_negotiates_h2_or_http1_by_alpn() {
    let certs = certs();
    let both = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let response = get(&both, "/status/200", Mode::Tls, &certs).await;
    assert_eq!(response.version, Version::HTTP_2);
    let http1 = TestServer::https(&certs, Tls::Http1Only).await;
    let response = get(&http1, "/status/200", Mode::Tls, &certs).await;
    assert_eq!(response.version, Version::HTTP_11);
}

#[tokio::test]
async fn mtls_requires_the_client_certificate() {
    let certs = certs();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let ok = get(&server, "/status/200", Mode::TlsWithClientCert, &certs).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(
        try_get(&server, "/status/200", Mode::Tls, &certs)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn self_signed_is_not_trusted_by_the_test_ca() {
    let certs = certs();
    let server = TestServer::https(&certs, Tls::SelfSigned).await;
    let err = try_get(&server, "/status/200", Mode::Tls, &certs)
        .await
        .err()
        .unwrap();
    assert!(err.contains("certificate"), "{err}");
}

#[tokio::test]
async fn echo_reports_the_request() {
    let certs = certs();
    let server = TestServer::http().await;
    let response = get(&server, "/echo?a=1", Mode::Http1, &certs).await;
    let json: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(json["method"], "GET");
    assert_eq!(json["path"], "/echo?a=1");
    assert_eq!(json["version"], "HTTP/1.1");
    let host = json["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h[0] == "host")
        .unwrap();
    assert_eq!(host[1], format!("localhost:{}", server.port()));
}

#[tokio::test]
async fn delay_waits_before_answering() {
    let certs = certs();
    let server = TestServer::http().await;
    let started = Instant::now();
    let response = get(&server, "/delay/150", Mode::Http1, &certs).await;
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(&response.body[..], b"done");
    let started = Instant::now();
    let response = get(&server, "/slow-body/150", Mode::Http1, &certs).await;
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(&response.body[..], b"finally");
}

#[tokio::test]
async fn redirects_point_where_asked() {
    let certs = certs();
    let server = TestServer::http().await;
    let response = get(&server, "/redirect/307?to=%2Fecho", Mode::Http1, &certs).await;
    assert_eq!(response.status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers["location"], "/echo");
    let response = get(&server, "/loop", Mode::Http1, &certs).await;
    assert_eq!(response.status, StatusCode::FOUND);
    assert_eq!(response.headers["location"], "/loop");
}

#[tokio::test]
async fn large_bodies_are_streamed_in_full() {
    let certs = certs();
    let server = TestServer::http().await;
    let len = 3 * 1024 * 1024 + 7;
    let response = get(&server, &format!("/bytes/{len}"), Mode::Http1, &certs).await;
    assert_eq!(response.body.len(), len);
    assert_eq!(response.headers["content-length"], len.to_string());
    assert_eq!(response.body[251], 0);
}

#[tokio::test]
async fn compressed_routes_decode_to_the_text() {
    let certs = certs();
    let server = TestServer::http().await;
    for encoding in ["gzip", "br", "zstd", "deflate"] {
        let response = get(
            &server,
            &format!("/compressed/{encoding}"),
            Mode::Http1,
            &certs,
        )
        .await;
        assert_eq!(response.headers["content-encoding"], encoding);
        assert_ne!(&response.body[..], COMPRESSED_TEXT.as_bytes());
        assert_eq!(decode(encoding, &response.body).await, COMPRESSED_TEXT);
    }
}

async fn decode(encoding: &str, body: &[u8]) -> String {
    let mut out = String::new();
    match encoding {
        "gzip" => GzipDecoder::new(body).read_to_string(&mut out).await,
        "br" => BrotliDecoder::new(body).read_to_string(&mut out).await,
        "zstd" => ZstdDecoder::new(body).read_to_string(&mut out).await,
        _ => ZlibDecoder::new(body).read_to_string(&mut out).await,
    }
    .unwrap();
    out
}

#[tokio::test]
async fn png_is_served_byte_for_byte() {
    let certs = certs();
    let server = TestServer::http().await;
    let response = get(&server, "/png", Mode::Http1, &certs).await;
    assert_eq!(response.headers["content-type"], "image/png");
    assert_eq!(&response.body[..], PNG);
}

#[tokio::test]
async fn a_stalled_server_never_answers() {
    let certs = certs();
    let server = TestServer::stalled("http").await;
    let attempt = get(&server, "/echo", Mode::Http1, &certs);
    let result = tokio::time::timeout(Duration::from_millis(200), attempt).await;
    assert!(result.is_err());
}
