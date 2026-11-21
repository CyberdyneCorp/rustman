//! Scenario "SCRAM over TLS" (add-messaging-client task 2.3) without a
//! real broker: an in-process Kafka listener behind rustls answers the
//! version sync and the SASL handshake, records what the client sends
//! inside the TLS session, and rejects the SCRAM client-first message.
//! The client must complete the TLS handshake against the custom CA,
//! offer SCRAM-SHA-512 for the configured user, and report the failure
//! as `kafka: authentication failed (SCRAM-SHA-512)` without the password.
//! The full exchange against Redpanda runs in the CI `messaging-kafka` job.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, KAFKA_PW};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{Tls, server_config};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const API_VERSIONS: i16 = 18;
const SASL_HANDSHAKE: i16 = 17;
const SASL_AUTHENTICATE: i16 = 36;
const SASL_AUTHENTICATION_FAILED: i16 = 58;

/// What the client sent inside the TLS session.
#[derive(Default)]
struct Seen {
    mechanisms: Vec<String>,
    auth: Vec<Vec<u8>>,
}

type Shared = Arc<Mutex<Seen>>;

/// One request: its API key, correlation id and the bytes after the
/// fixed header fields (client id onwards).
struct Frame {
    api_key: i16,
    correlation: i32,
    rest: Vec<u8>,
}

async fn read_frame(stream: &mut (impl AsyncRead + Unpin)) -> Option<Frame> {
    let size = stream.read_i32().await.ok()?;
    let mut body = vec![0; usize::try_from(size).ok()?];
    stream.read_exact(&mut body).await.ok()?;
    let api_key = i16::from_be_bytes([body[0], body[1]]);
    let correlation = i32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    Some(Frame {
        api_key,
        correlation,
        rest: body[8..].to_vec(),
    })
}

/// The request body after the nullable client id string (header v1).
fn after_client_id(rest: &[u8]) -> &[u8] {
    let len = i16::from_be_bytes([rest[0], rest[1]]);
    &rest[2 + usize::try_from(len.max(0)).unwrap()..]
}

fn string(bytes: &[u8]) -> String {
    let len = usize::try_from(i16::from_be_bytes([bytes[0], bytes[1]])).unwrap();
    String::from_utf8_lossy(&bytes[2..2 + len]).into_owned()
}

/// ApiVersions v3: no error, the keys this listener speaks, no throttle.
fn api_versions() -> Vec<u8> {
    let keys: [(i16, i16, i16); 3] = [
        (API_VERSIONS, 0, 3),
        (SASL_HANDSHAKE, 1, 1),
        (SASL_AUTHENTICATE, 0, 0),
    ];
    let mut out = 0i16.to_be_bytes().to_vec();
    out.push(u8::try_from(keys.len() + 1).unwrap());
    for (key, min, max) in keys {
        out.extend(key.to_be_bytes());
        out.extend(min.to_be_bytes());
        out.extend(max.to_be_bytes());
        out.push(0);
    }
    out.extend(0i32.to_be_bytes());
    out.push(0);
    out
}

/// SaslHandshake v1: no error, SCRAM-SHA-512 offered.
fn sasl_handshake() -> Vec<u8> {
    let mechanism = b"SCRAM-SHA-512";
    let mut out = 0i16.to_be_bytes().to_vec();
    out.extend(1i32.to_be_bytes());
    out.extend(i16::try_from(mechanism.len()).unwrap().to_be_bytes());
    out.extend(mechanism);
    out
}

/// SaslAuthenticate v0: authentication failed.
fn sasl_rejected() -> Vec<u8> {
    let message = b"Authentication failed";
    let mut out = SASL_AUTHENTICATION_FAILED.to_be_bytes().to_vec();
    out.extend(i16::try_from(message.len()).unwrap().to_be_bytes());
    out.extend(message);
    out.extend(0i32.to_be_bytes());
    out
}

fn answer(frame: &Frame, seen: &Shared) -> Option<Vec<u8>> {
    match frame.api_key {
        API_VERSIONS => Some(api_versions()),
        SASL_HANDSHAKE => {
            let mechanism = string(after_client_id(&frame.rest));
            seen.lock().unwrap().mechanisms.push(mechanism);
            Some(sasl_handshake())
        }
        SASL_AUTHENTICATE => {
            let body = after_client_id(&frame.rest);
            let len = usize::try_from(i32::from_be_bytes([body[0], body[1], body[2], body[3]]));
            let bytes = body[4..4 + len.unwrap()].to_vec();
            seen.lock().unwrap().auth.push(bytes);
            Some(sasl_rejected())
        }
        _ => None,
    }
}

async fn serve(mut stream: impl AsyncRead + AsyncWrite + Unpin, seen: Shared) {
    while let Some(frame) = read_frame(&mut stream).await {
        let Some(body) = answer(&frame, &seen) else {
            return;
        };
        let size = i32::try_from(body.len() + 4).unwrap();
        let mut out = size.to_be_bytes().to_vec();
        out.extend(frame.correlation.to_be_bytes());
        out.extend(body);
        if stream.write_all(&out).await.is_err() {
            return;
        }
    }
}

/// A TLS Kafka listener on `localhost`; returns its port.
async fn start(certs: &TestCerts, seen: Shared) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(certs, Tls::Http1Only)));
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(tcp).await {
                    serve(tls, seen).await;
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn scram_sha_512_runs_inside_the_tls_session_and_failures_hide_the_password() {
    let certs = TestCerts::generate();
    let seen = Shared::default();
    let port = start(&certs, seen.clone()).await;
    let fixture = Fixture::new();
    fixture.write("tls/ca.pem", &certs.ca_pem);
    let text = format!(
        "version: 1\nkind: kafka\nname: Watch\noperation: consume\nbrokers: localhost:{port}\ntopic: orders\nsecurity:\n  protocol: SASL_SSL\n  mechanism: SCRAM-SHA-512\n  username: app\n  password: '{{{{kafka_pw}}}}'\ntls: {{ca_bundle: tls/ca.pem}}\n"
    );
    let resolved = fixture.resolve(&text, "", KAFKA_PW);
    let mut options = rustman_messaging::RunOptions::headless(&resolved);
    options.connect_timeout = Duration::from_secs(3);
    let err = rustman_messaging::run(&resolved, options)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(err, "kafka: authentication failed (SCRAM-SHA-512)");
    assert!(!err.contains(KAFKA_PW));
    let seen = seen.lock().unwrap();
    assert!(
        seen.mechanisms.iter().all(|m| m == "SCRAM-SHA-512") && !seen.mechanisms.is_empty(),
        "{:?}",
        seen.mechanisms
    );
    let first = String::from_utf8_lossy(&seen.auth[0]).into_owned();
    assert!(first.starts_with("n,,n=app,r="), "{first}");
    assert!(!first.contains(KAFKA_PW), "{first}");
}
