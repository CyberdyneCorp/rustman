//! Digest, AWS SigV4 and JWT auth through the engine (runner-parity tasks
//! 3.1-3.6), against in-process servers only.

mod support;

use std::time::Duration;

use rustman_core::model::{ApiKeyLocation, Auth, AwsCredentials, AwsSigV4, Jwt, JwtAlgorithm};
use rustman_core::vars::{ResolvedBody, ResolvedRequest};
use rustman_core::{Secret, SecretStore};
use rustman_http::{Exchange, HttpEngine, Login, OAuth2Context, RequestError, SendOptions};
use rustman_test_support::DigestServer;
use rustman_test_support::digest::{PASSWORD, USER};
use support::TestServer;
use support::recorder::Recorder;
use support::request::{echoed_header, file, get, pair};

struct Sender {
    engine: HttpEngine,
    _dir: tempfile::TempDir,
    store: SecretStore,
}

impl Sender {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::with_keychain(dir.path(), None);
        Self {
            engine: HttpEngine::new(),
            _dir: dir,
            store,
        }
    }

    async fn send(&self, request: &mut ResolvedRequest) -> Result<Exchange, RequestError> {
        self.send_in(request, Some("staging")).await
    }

    async fn send_in(
        &self,
        request: &mut ResolvedRequest,
        env: Option<&str>,
    ) -> Result<Exchange, RequestError> {
        self.send_with(request, env, &SendOptions::default()).await
    }

    async fn send_with(
        &self,
        request: &mut ResolvedRequest,
        env: Option<&str>,
        options: &SendOptions,
    ) -> Result<Exchange, RequestError> {
        let context = OAuth2Context {
            store: &self.store,
            env,
            cache_key: "r".into(),
            login: Login::NonInteractive {
                request: "r".into(),
            },
        };
        self.engine
            .send_authorized(request, options, &context)
            .await
    }
}

fn digest(url: &str, password: &str) -> ResolvedRequest {
    let mut request = get(url);
    request.auth = Some(Auth::Digest {
        username: USER.into(),
        password: password.into(),
    });
    request
}

/// The `name=value` field of a Digest `Authorization` header.
fn field<'a>(header: &'a str, name: &str) -> &'a str {
    let (_, rest) = header
        .split_once(&format!(" {name}="))
        .unwrap_or_else(|| panic!("no {name} in {header}"));
    let value = rest.split(',').next().unwrap();
    value.trim_matches('"')
}

#[tokio::test]
async fn digest_answers_the_challenge_and_retries_once() {
    let server = DigestServer::start("SHA-256").await;
    let sender = Sender::new();
    let exchange = sender
        .send(&mut digest(&server.url("/data"), PASSWORD))
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    let hits = server.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        (hits[0].authorization.as_deref(), hits[0].status),
        (None, 401)
    );
    let answer = hits[1].authorization.clone().unwrap();
    assert!(answer.starts_with("Digest "), "{answer}");
    assert_eq!(field(&answer, "algorithm"), "SHA-256");
    assert_eq!(field(&answer, "qop"), "auth");
    assert_eq!(field(&answer, "nc"), "00000001");
    assert_eq!(field(&answer, "cnonce").len(), 32);
    let auth = exchange.timings.auth.expect("the challenge round trip");
    assert!(exchange.timings.total >= auth, "{:?}", exchange.timings);
}

#[tokio::test]
async fn every_supported_algorithm_is_accepted() {
    for algorithm in ["MD5", "MD5-sess", "SHA-256", "SHA-256-sess"] {
        let server = DigestServer::start(algorithm).await;
        let exchange = Sender::new()
            .send(&mut digest(&server.url("/x?q=1"), PASSWORD))
            .await
            .unwrap();
        assert_eq!(exchange.status, 200, "{algorithm}");
    }
}

#[tokio::test]
async fn a_wrong_password_ends_with_the_second_401() {
    let server = DigestServer::start("MD5").await;
    let exchange = Sender::new()
        .send(&mut digest(&server.url("/data"), "wrong"))
        .await
        .unwrap();
    assert_eq!(exchange.status, 401);
    assert_eq!(server.hits().len(), 2);
}

#[tokio::test]
async fn later_requests_reuse_the_nonce_with_the_next_count() {
    let server = DigestServer::start("SHA-256").await;
    let sender = Sender::new();
    sender
        .send(&mut digest(&server.url("/a"), PASSWORD))
        .await
        .unwrap();
    let second = sender
        .send(&mut digest(&server.url("/b"), PASSWORD))
        .await
        .unwrap();
    assert_eq!(second.status, 200);
    assert_eq!(second.timings.auth, None);
    let hits = server.hits();
    assert_eq!(hits.len(), 3, "{hits:?}");
    assert_eq!(
        field(hits[2].authorization.as_deref().unwrap(), "nc"),
        "00000002"
    );
    // Another environment has no cached nonce.
    sender
        .send_in(&mut digest(&server.url("/c"), PASSWORD), Some("prod"))
        .await
        .unwrap();
    assert_eq!(server.hits()[3].authorization, None);
}

#[tokio::test]
async fn a_stale_nonce_is_replaced_with_one_retry() {
    let server = DigestServer::start("MD5").await;
    let sender = Sender::new();
    sender
        .send(&mut digest(&server.url("/a"), PASSWORD))
        .await
        .unwrap();
    server.rotate_nonce();
    let exchange = sender
        .send(&mut digest(&server.url("/b"), PASSWORD))
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    let hits = server.hits();
    assert_eq!(hits.len(), 4, "{hits:?}");
    assert_eq!(hits[2].status, 401);
    let retry = hits[3].authorization.as_deref().unwrap();
    assert_eq!(field(retry, "nonce"), "nonce-2");
    assert_eq!(field(retry, "nc"), "00000001");
}

#[tokio::test]
async fn auth_int_challenges_are_refused() {
    let server = DigestServer::start_with("MD5", "auth-int").await;
    let err = Sender::new()
        .send(&mut digest(&server.url("/a"), PASSWORD))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "digest: unsupported qop auth-int");
}

#[tokio::test]
async fn a_body_too_large_to_replay_is_never_sent() {
    let server = DigestServer::start("MD5").await;
    let dir = tempfile::tempdir().unwrap();
    let big = std::fs::File::create(dir.path().join("big.bin")).unwrap();
    big.set_len(50 * 1024 * 1024).unwrap();
    let mut request = digest(&server.url("/upload"), PASSWORD);
    request.method = "PUT".into();
    let path = dir.path().join("big.bin");
    request.body = ResolvedBody::File(rustman_core::vars::ResolvedFile {
        raw: "big.bin".into(),
        path,
    });
    let err = Sender::new().send(&mut request).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "digest: body larger than 10 MiB cannot be replayed"
    );
    assert!(server.hits().is_empty());
    assert_eq!(server.body_bytes(), 0);
    // A small file body is buffered and sent twice.
    let mut small = digest(&server.url("/upload"), PASSWORD);
    small.method = "PUT".into();
    small.body = ResolvedBody::File(file(dir.path(), "small.bin", "0123456789").unwrap());
    assert_eq!(Sender::new().send(&mut small).await.unwrap().status, 200);
    assert_eq!(server.body_bytes(), 20);
}

fn sigv4(url: &str, service: &str) -> ResolvedRequest {
    let mut request = get(url);
    request.auth = Some(Auth::AwsSigV4(Box::new(AwsSigV4 {
        region: "us-east-1".into(),
        service: service.into(),
        credentials: AwsCredentials::Fields {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SECRET_KEY.into(),
            session_token: Some("SESSION-TOKEN-CANARY".into()),
        },
        unsigned_payload: false,
    })));
    request
}

#[tokio::test]
async fn sigv4_signs_with_the_default_headers() {
    let server = TestServer::http().await;
    let mut request = sigv4(&server.url("/echo"), "s3");
    request.headers = vec![pair("X-Trace", "1")];
    let exchange = Sender::new().send(&mut request).await.unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    let authorization = echoed_header(&echo, "authorization").remove(0);
    let prefix = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/";
    assert!(authorization.starts_with(prefix), "{authorization}");
    assert!(authorization.contains("/us-east-1/s3/aws4_request, SignedHeaders="));
    let signed = field(&authorization, "SignedHeaders");
    let signed: Vec<&str> = signed.split(';').collect();
    for header in [
        "host",
        "x-amz-content-sha256",
        "x-amz-date",
        "x-amz-security-token",
        "x-trace",
    ] {
        assert!(signed.contains(&header), "{signed:?}");
    }
    assert_eq!(echoed_header(&echo, "x-amz-date")[0].len(), 16);
    // SHA-256 of the empty body.
    assert_eq!(
        echoed_header(&echo, "x-amz-content-sha256"),
        ["e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"]
    );
    let raw = String::from_utf8_lossy(&exchange.body).into_owned();
    assert!(raw.contains("SESSION-TOKEN-CANARY"));
    let shown = exchange.redactor(&request).text(&raw).into_owned();
    assert!(!shown.contains("SESSION-TOKEN-CANARY"), "{shown}");
}

#[tokio::test]
async fn sigv4_unsigned_payload_skips_the_body_hash() {
    let server = TestServer::http().await;
    let mut request = sigv4(&server.url("/echo"), "s3");
    request.method = "PUT".into();
    request.body = ResolvedBody::Text(rustman_core::vars::Resolved::literal("data"));
    if let Some(Auth::AwsSigV4(config)) = &mut request.auth {
        config.unsigned_payload = true;
    }
    let exchange = Sender::new().send(&mut request).await.unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(
        echoed_header(&echo, "x-amz-content-sha256"),
        ["UNSIGNED-PAYLOAD"]
    );
    assert_eq!(echo["body"], "data");
}

#[tokio::test]
async fn every_redirect_hop_is_signed_again() {
    let recorder = Recorder::start().await;
    let mut request = sigv4(&recorder.url("/redirect?status=307&to=/ok"), "execute-api");
    let exchange = Sender::new().send(&mut request).await.unwrap();
    assert_eq!(exchange.status, 200);
    let hits = recorder.hits();
    assert_eq!(hits.len(), 2);
    let signature = |i: usize| {
        let header = hits[i].authorization.clone().unwrap();
        field(&header, "Signature").to_owned()
    };
    assert_eq!(signature(0).len(), 64);
    assert_ne!(signature(0), signature(1));
}

const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SigV4 signature of a received `GET <path>` without a query, computed
/// independently of the signer (canonical request, string to sign, HMAC
/// chain), from the headers the server received.
fn expected_signature(headers: &http::HeaderMap, path: &str, scope: &str) -> String {
    use ring::{digest, hmac};
    let authorization = headers["authorization"].to_str().unwrap();
    let signed = field(authorization, "SignedHeaders");
    let canonical_headers: String = signed
        .split(';')
        .map(|name| format!("{name}:{}\n", headers[name].to_str().unwrap().trim()))
        .collect();
    let payload = headers["x-amz-content-sha256"].to_str().unwrap();
    let canonical = format!("GET\n{path}\n\n{canonical_headers}\n{signed}\n{payload}");
    let date_time = headers["x-amz-date"].to_str().unwrap();
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date_time}\n{}/{scope}/aws4_request\n{}",
        &date_time[..8],
        hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    );
    let mac = |key: &[u8], data: &str| {
        let key = hmac::Key::new(hmac::HMAC_SHA256, key);
        hmac::sign(&key, data.as_bytes()).as_ref().to_vec()
    };
    let mut key = format!("AWS4{SECRET_KEY}").into_bytes();
    for part in [&date_time[..8], "us-east-1", "execute-api", "aws4_request"] {
        key = mac(&key, part);
    }
    hex(&mac(&key, &to_sign))
}

/// A same-origin hop is signed again, validly, for its own URL.
#[tokio::test]
async fn a_same_origin_redirect_is_signed_for_its_own_url() {
    let recorder = Recorder::start().await;
    let mut request = sigv4(&recorder.url("/redirect?status=302&to=/ok"), "execute-api");
    let exchange = Sender::new().send(&mut request).await.unwrap();
    assert_eq!(exchange.status, 200);
    let hits = recorder.hits();
    assert_eq!(hits[1].path, "/ok");
    let headers = &hits[1].headers;
    let authorization = headers["authorization"].to_str().unwrap();
    assert_eq!(
        field(authorization, "Signature"),
        expected_signature(headers, "/ok", "us-east-1/execute-api")
    );
    assert_eq!(
        headers["host"],
        recorder.url("").trim_start_matches("http://")
    );
}

/// Trace headers are injected before signing, so the signature covers
/// `traceparent` (add-trace-propagation task 2.1).
#[tokio::test]
async fn sigv4_signs_the_injected_traceparent() {
    let recorder = Recorder::start().await;
    let mut request = sigv4(&recorder.url("/ok"), "execute-api");
    let options = SendOptions {
        trace: Some(rustman_core::trace::TracePropagation::default()),
        ..SendOptions::default()
    };
    let sender = Sender::new();
    let exchange = sender
        .send_with(&mut request, Some("staging"), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    let hits = recorder.hits();
    let headers = &hits[0].headers;
    let authorization = headers["authorization"].to_str().unwrap();
    let signed = field(authorization, "SignedHeaders");
    assert!(signed.split(';').any(|h| h == "traceparent"), "{signed}");
    let traceparent = headers["traceparent"].to_str().unwrap();
    assert!(traceparent.contains(&exchange.trace.unwrap().context.trace_id.to_string()));
    assert_eq!(
        field(authorization, "Signature"),
        expected_signature(headers, "/ok", "us-east-1/execute-api")
    );
}

/// Regression: the engine signed every hop, so a redirect to another
/// origin received a signature computed with the request's credentials,
/// its access key and its session token.
#[tokio::test]
async fn a_cross_origin_redirect_is_never_signed() {
    let target = Recorder::start().await;
    let origin = Recorder::start().await;
    for status in [302, 307] {
        let back = origin.url("/ok");
        let to = format!("{}?to={back}", target.url("/redirect"));
        let path = format!("/redirect?status={status}&to={}", urlencode(&to));
        let mut request = sigv4(&origin.url(&path), "execute-api");
        let exchange = Sender::new().send(&mut request).await.unwrap();
        assert_eq!(exchange.status, 200, "{status}");
        assert_eq!(exchange.redirects.len(), 2, "{status}");
    }
    let signed = origin.hits();
    assert!(signed[0].authorization.is_some(), "the first hop is signed");
    for hit in target
        .hits()
        .iter()
        .chain(&signed[1..2])
        .chain(&signed[3..])
    {
        let names: Vec<&str> = hit.headers.keys().map(http::HeaderName::as_str).collect();
        assert!(hit.authorization.is_none(), "{} got {names:?}", hit.path);
        assert!(
            !names.iter().any(|n| n.starts_with("x-amz-")),
            "{} got {names:?}",
            hit.path
        );
        let raw = format!("{:?}", hit.headers);
        for secret in ["AKIDEXAMPLE", "SESSION-TOKEN-CANARY", SECRET_KEY] {
            assert!(!raw.contains(secret), "{} got {raw}", hit.path);
        }
    }
    assert_eq!(target.hits().len(), 2);
    assert_eq!(signed.len(), 4, "back on the origin, still unsigned");
}

fn urlencode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

#[tokio::test]
async fn a_clock_skew_rejection_gets_a_hint() {
    let server = TestServer::http().await;
    let mut request = sigv4(&server.url("/aws-skew"), "s3");
    let exchange = Sender::new().send(&mut request).await.unwrap();
    assert_eq!(exchange.status, 403);
    let hint = exchange
        .warnings
        .iter()
        .find(|w| w.starts_with("check the system clock; request was signed at "))
        .unwrap_or_else(|| panic!("{:?}", exchange.warnings));
    assert_eq!(
        hint.len(),
        "check the system clock; request was signed at ".len() + 16
    );
}

fn jwt(url: &str) -> ResolvedRequest {
    let mut request = get(url);
    request.auth = Some(Auth::Jwt(Box::new(Jwt {
        algorithm: JwtAlgorithm::HS256,
        secret: Some("jwt-secret-CANARY".into()),
        private_key: None,
        private_key_file: None,
        claims: serde_json::json!({"sub": "42", "scope": "read"})
            .as_object()
            .unwrap()
            .clone(),
        header: serde_json::Map::new(),
        ttl_secs: 300,
        location: ApiKeyLocation::Header,
        name: None,
    })));
    request.secrets = vec![Secret::new("jwt_secret", "jwt-secret-CANARY".to_owned())];
    request
}

/// The payload of a compact JWT.
fn payload(token: &str) -> serde_json::Value {
    use base64::Engine as _;
    let part = token.split('.').nth(1).unwrap();
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}

#[tokio::test]
async fn jwt_signs_a_new_bearer_token_per_request() {
    let server = TestServer::http().await;
    let sender = Sender::new();
    let mut tokens = Vec::new();
    for round in 0..2 {
        if round == 1 {
            // `iat` has a resolution of one second.
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        let mut request = jwt(&server.url("/echo"));
        let exchange = sender.send(&mut request).await.unwrap();
        let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
        let header = echoed_header(&echo, "authorization").remove(0);
        let token = header.strip_prefix("Bearer ").unwrap().to_owned();
        let claims = payload(&token);
        assert_eq!(
            (&claims["sub"], &claims["scope"]),
            (&"42".into(), &"read".into())
        );
        let iat = claims["iat"].as_u64().unwrap();
        assert_eq!(claims["exp"].as_u64(), Some(iat + 300));
        let shown = exchange
            .redactor(&request)
            .text(&String::from_utf8_lossy(&exchange.body))
            .into_owned();
        assert!(!shown.contains(&token), "{shown}");
        assert!(shown.contains("Bearer «redacted:jwt»"), "{shown}");
        tokens.push(token);
    }
    assert_ne!(tokens[0], tokens[1]);
}

#[tokio::test]
async fn jwt_can_go_in_a_named_header_or_the_query() {
    let server = TestServer::http().await;
    let mut request = jwt(&server.url("/echo"));
    if let Some(Auth::Jwt(config)) = &mut request.auth {
        config.name = Some("X-Token".into());
    }
    let exchange = Sender::new().send(&mut request).await.unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echoed_header(&echo, "x-token")[0].split('.').count(), 3);
    assert!(echoed_header(&echo, "authorization").is_empty());

    let mut request = jwt(&server.url("/echo"));
    if let Some(Auth::Jwt(config)) = &mut request.auth {
        config.location = ApiKeyLocation::Query;
        config.name = Some("access_token".into());
    }
    let exchange = Sender::new().send(&mut request).await.unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    let path = echo["path"].as_str().unwrap();
    assert!(path.starts_with("/echo?access_token=ey"), "{path}");
}

#[tokio::test]
async fn jwt_errors_are_request_errors() {
    let server = TestServer::http().await;
    let mut request = jwt(&server.url("/echo"));
    if let Some(Auth::Jwt(config)) = &mut request.auth {
        config.algorithm = JwtAlgorithm::RS256;
        config.secret = None;
        config.private_key = Some("not a key".into());
    }
    let err = Sender::new().send(&mut request).await.unwrap_err();
    assert_eq!(err.to_string(), "jwt: RS256 requires an RSA private key");
}
