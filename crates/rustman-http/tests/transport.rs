//! The timed connector, the hyper client and timeouts (tasks 4.2 and 4.3).

mod support;

use std::time::{Duration, Instant};

use rustman_core::Secret;
use rustman_core::model::RequestSettings;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, SecretSpan, TlsFiles,
};
use rustman_http::{HttpEngine, HttpVersion, RequestError, SendOptions, Timings, TlsSettings};
use support::{TestCerts, TestServer, Tls};

fn get(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "GET".into(),
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

fn pair(name: &str, value: &str) -> ResolvedPair {
    ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    }
}

/// Default options that trust the test CA.
fn trusting(certs: &TestCerts) -> SendOptions {
    SendOptions {
        tls: TlsSettings {
            ca_bundle: Some(certs.ca_pem.clone().into_bytes()),
            ..TlsSettings::default()
        },
        ..SendOptions::default()
    }
}

fn with_timeouts(total_ms: u64, connect_ms: Option<u64>) -> SendOptions {
    let settings = RequestSettings {
        timeout_ms: Some(total_ms),
        connect_timeout_ms: connect_ms,
        ..RequestSettings::default()
    };
    SendOptions::new(&Default::default(), &settings)
}

fn connection_phases(t: &Timings) -> [Option<Duration>; 3] {
    [t.dns, t.connect, t.tls]
}

#[tokio::test]
async fn plain_http_uses_http1_and_has_no_tls_phase() {
    let server = TestServer::http().await;
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/204")), &SendOptions::default())
        .await
        .unwrap();
    assert_eq!(exchange.http_version, HttpVersion::Http11);
    assert_eq!(exchange.status, 204);
    let t = exchange.timings;
    assert!(t.dns.is_some() && t.connect.is_some());
    assert_eq!(t.tls, None);
    assert!(!exchange.insecure);
    let json = serde_json::to_value(t).unwrap();
    assert!(json["tls"].is_null());
}

#[tokio::test]
async fn an_ip_literal_has_no_dns_phase() {
    let server = TestServer::http().await;
    let url = format!("http://{}/status/200", server.addr());
    let exchange = HttpEngine::new()
        .send(&get(&url), &SendOptions::default())
        .await
        .unwrap();
    assert_eq!(exchange.timings.dns, None);
    assert!(exchange.timings.connect.is_some());
}

#[tokio::test]
async fn h2c_with_prior_knowledge_uses_http2() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.settings.http2_prior_knowledge = Some(true);
    let options = SendOptions::new(&Default::default(), &request.settings);
    let exchange = HttpEngine::new().send(&request, &options).await.unwrap();
    assert_eq!(exchange.http_version, HttpVersion::Http2);
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echo["version"], "HTTP/2.0");
}

#[tokio::test]
async fn https_negotiates_h2_and_times_every_phase() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/echo")), &trusting(&certs))
        .await
        .unwrap();
    assert_eq!(exchange.http_version, HttpVersion::Http2);
    let t = exchange.timings;
    let [dns, connect, tls] = connection_phases(&t).map(Option::unwrap);
    assert!(
        t.total >= dns + connect + tls + t.ttfb + t.download,
        "{t:?}"
    );
    let json = serde_json::to_value(t).unwrap();
    for phase in ["dns", "connect", "tls", "ttfb", "download", "total"] {
        assert!(json[phase].is_f64(), "{phase}: {json}");
    }
}

#[tokio::test]
async fn https_falls_back_to_http1() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::Http1Only).await;
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &trusting(&certs))
        .await
        .unwrap();
    assert_eq!(exchange.http_version, HttpVersion::Http11);
    assert!(exchange.timings.tls.is_some());
}

#[tokio::test]
async fn a_reused_h2_connection_has_null_connection_phases() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let engine = HttpEngine::new();
    let options = trusting(&certs);
    let request = get(&server.url("/status/200"));
    let first = engine.send(&request, &options).await.unwrap();
    assert!(
        connection_phases(&first.timings)
            .iter()
            .all(Option::is_some)
    );
    let second = engine.send(&request, &options).await.unwrap();
    assert_eq!(connection_phases(&second.timings), [None, None, None]);
    assert!(second.timings.total >= second.timings.ttfb);
}

#[tokio::test]
async fn a_reused_http1_connection_has_null_connection_phases() {
    let server = TestServer::http().await;
    let engine = HttpEngine::new();
    let request = get(&server.url("/status/200"));
    let options = SendOptions::default();
    engine.send(&request, &options).await.unwrap();
    // The client returns an HTTP/1.1 connection to its pool in the background.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second = engine.send(&request, &options).await.unwrap();
    assert_eq!(connection_phases(&second.timings), [None, None, None]);
}

#[tokio::test]
async fn insecure_requests_are_flagged() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::SelfSigned).await;
    let mut options = SendOptions::default();
    options.tls.verify = false;
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert!(exchange.insecure);
}

#[tokio::test]
async fn an_untrusted_certificate_is_a_tls_error() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::SelfSigned).await;
    let err = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &trusting(&certs))
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Tls(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with("tls: certificate verification failed"),
        "{err}"
    );
}

#[tokio::test]
async fn requests_carry_defaults_query_and_body() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.query = vec![pair("q", "a b")];
    request.headers = vec![pair("X-Trace", "1"), pair("Content-Length", "3")];
    request.body = ResolvedBody::Json(serde_json::json!({"a": 1}));
    let exchange = HttpEngine::new()
        .send(&request, &SendOptions::default())
        .await
        .unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["path"], "/echo?q=a+b");
    assert_eq!(echo["body"], r#"{"a":1}"#);
    let header = |name: &str| -> Vec<String> {
        echo["headers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h[0] == name)
            .map(|h| h[1].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(header("user-agent"), [rustman_http::USER_AGENT_VALUE]);
    assert_eq!(header("accept"), ["*/*"]);
    assert_eq!(header("content-type"), ["application/json"]);
    assert_eq!(header("content-length"), ["7"]);
    assert_eq!(header("x-trace"), ["1"]);
}

#[tokio::test]
async fn an_explicit_content_type_wins() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.headers = vec![pair("Content-Type", "application/vnd.api+json")];
    request.body = ResolvedBody::Json(serde_json::json!({}));
    let exchange = HttpEngine::new()
        .send(&request, &SendOptions::default())
        .await
        .unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    let types: Vec<_> = echo["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h[0] == "content-type")
        .collect();
    assert_eq!(types.len(), 1);
    assert_eq!(types[0][1], "application/vnd.api+json");
}

#[tokio::test]
async fn a_slow_server_times_out_in_the_response_phase() {
    let server = TestServer::http().await;
    let started = Instant::now();
    let err = HttpEngine::new()
        .send(&get(&server.url("/delay/2000")), &with_timeouts(500, None))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(err.to_string(), "timeout after 500 ms (response)");
    // The spec scenario's bound: the timeout itself plus 100 ms of slack.
    assert!(elapsed < Duration::from_millis(600), "{elapsed:?}");
}

#[tokio::test]
async fn a_slow_body_times_out_in_the_response_phase() {
    let server = TestServer::http().await;
    let err = HttpEngine::new()
        .send(
            &get(&server.url("/slow-body/2000")),
            &with_timeouts(300, None),
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 300 ms (response)");
}

#[tokio::test]
async fn a_stalled_handshake_hits_the_connect_timeout() {
    let server = TestServer::stalled("https").await;
    let started = Instant::now();
    let err = HttpEngine::new()
        .send(&get(&server.url("/")), &with_timeouts(30_000, Some(300)))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 300 ms (connect)");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn the_total_timeout_while_connecting_is_a_connect_timeout() {
    let server = TestServer::stalled("https").await;
    let err = HttpEngine::new()
        .send(&get(&server.url("/")), &with_timeouts(300, None))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 300 ms (connect)");
}

#[tokio::test]
async fn the_timeout_phase_is_per_request_on_a_shared_transport() {
    let stalled = TestServer::stalled("https").await;
    let slow = TestServer::http().await;
    let engine = std::sync::Arc::new(HttpEngine::new());
    let options = with_timeouts(3_000, Some(3_000));
    let connecting = {
        let engine = engine.clone();
        let (request, options) = (get(&stalled.url("/")), options.clone());
        tokio::spawn(async move { engine.send(&request, &options).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut short = options.clone();
    short.timeout = Duration::from_millis(500);
    let err = engine
        .send(&get(&slow.url("/delay/2000")), &short)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 500 ms (response)");
    let err = connecting.await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "timeout after 3000 ms (connect)");
}

#[tokio::test]
async fn a_refused_connection_is_a_network_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let err = HttpEngine::new()
        .send(
            &get(&format!("http://127.0.0.1:{port}/")),
            &SendOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Network(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with(&format!("cannot connect to 127.0.0.1:{port}")),
        "{err}"
    );
}

#[tokio::test]
async fn error_messages_are_redacted() {
    let url = "http://localhost/a b/s3cr3t";
    let start = url.find("s3cr3t").unwrap();
    let mut request = get(url);
    request.url.secret_spans = vec![SecretSpan {
        name: "token".into(),
        range: start..start + 6,
    }];
    request.secrets = vec![Secret::new("token", "s3cr3t".to_owned())];
    let err = HttpEngine::new()
        .send(&request, &SendOptions::default())
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(!message.contains("s3cr3t"), "{message}");
    assert!(message.contains("«redacted:token»"), "{message}");
}
