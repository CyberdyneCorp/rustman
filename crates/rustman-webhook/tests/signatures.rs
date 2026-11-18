//! Signature verification on live captures (spec: GitHub, Stripe and
//! Generic HMAC signature verification), with secrets read from an
//! environment's secret store, and the canary check that a verifier key
//! never reaches a capture or the event log.

mod common;

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{raw, status};
use rustman_core::Environment;
use rustman_core::vars::{EnvironmentSource, Resolver, SecretLookup};
use rustman_webhook::signature::Algorithm;
use rustman_webhook::{
    Capture, CaptureStore, Event, Listener, Signature, Verifiers, WebhookSettings,
};
use time::OffsetDateTime;

const GH_SECRET: &str = "canary-gh-7c41e9";
const STRIPE_SECRET: &str = "canary-whsec-0b2d55";
const SHOP_SECRET: &str = "canary-shop-93aa10";

const SETTINGS: &str = r#"
[workspace]
name = "hooks"
version = 1

[[webhook.verifiers]]
path = "/hooks/github"
type = "github"
secret = "{{gh_hook}}"

[[webhook.verifiers]]
path = "/hooks/stripe"
type = "stripe"
secret = "{{stripe_whsec}}"

[[webhook.verifiers]]
path = "/hooks/shop/**"
type = "hmac"
header = "X-Signature"
encoding = "base64"
secret = "{{shop_secret}}"
"#;

/// The `local` environment's secret store.
struct Store;

impl SecretLookup for Store {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(match name {
            "gh_hook" => Some(GH_SECRET.to_owned()),
            "stripe_whsec" => Some(STRIPE_SECRET.to_owned()),
            "shop_secret" => Some(SHOP_SECRET.to_owned()),
            _ => None,
        })
    }
}

fn verifiers() -> Verifiers {
    let env = Environment {
        name: "local".into(),
        secrets: vec![
            "gh_hook".into(),
            "stripe_whsec".into(),
            "shop_secret".into(),
        ],
        ..Environment::default()
    };
    let resolver = Resolver::new().with(EnvironmentSource::new(&env, &Store));
    let settings = WebhookSettings::parse(SETTINGS).unwrap();
    Verifiers::resolve(&settings.verifiers, &resolver).unwrap()
}

struct Running {
    store: CaptureStore,
    addr: std::net::SocketAddr,
    events: tokio::sync::broadcast::Receiver<Event>,
    _handle: rustman_webhook::ListenerHandle,
}

async fn start() -> Running {
    let store = CaptureStore::default();
    let events = store.subscribe();
    let config = common::config(rustman_webhook::Limits::default());
    let listener = Listener::bind(&config, store.clone(), rustman_webhook::Mocks::empty())
        .await
        .unwrap()
        .with_verifiers(verifiers());
    Running {
        addr: listener.local_addr(),
        store,
        events,
        _handle: listener.spawn(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// POST `body` to `path` with `headers`, returning the capture.
async fn post(l: &Running, path: &str, headers: &[(&str, String)], body: &str) -> Arc<Capture> {
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    let response = raw(l.addr, request.as_bytes()).await;
    assert_eq!(status(&response), 200, "{response}");
    l.store.snapshot().last().cloned().unwrap()
}

fn github_signature(body: &str) -> String {
    let mac = Algorithm::Sha256.hmac(GH_SECRET.as_bytes(), &[body.as_bytes()]);
    format!("sha256={}", hex(&mac))
}

#[tokio::test]
async fn github_valid_tampered_and_missing() {
    let l = start().await;
    let body = r#"{"zen":"Keep it logically awesome."}"#;
    let signed = [("X-Hub-Signature-256", github_signature(body))];
    let valid = post(&l, "/hooks/github", &signed, body).await;
    assert_eq!(valid.signature, Some(Signature::Valid));
    let tampered = post(
        &l,
        "/hooks/github",
        &signed,
        r#"{"zen":"Keep it logically awful."}"#,
    )
    .await;
    assert_eq!(tampered.signature, Some(Signature::Invalid));
    let missing = post(&l, "/hooks/github", &[], body).await;
    assert_eq!(missing.signature, Some(Signature::Missing));
    let unverified = post(&l, "/elsewhere", &signed, body).await;
    assert_eq!(unverified.signature, None);
}

fn stripe_signature(t: i64, body: &str) -> String {
    let payload = [t.to_string().as_bytes(), b".", body.as_bytes()].concat();
    let mac = Algorithm::Sha256.hmac(STRIPE_SECRET.as_bytes(), &[&payload]);
    format!("t={t},v1={}", hex(&mac))
}

#[tokio::test]
async fn stripe_fresh_is_valid_and_ten_minutes_old_is_expired() {
    let l = start().await;
    let body = r#"{"id":"evt_1","type":"invoice.paid"}"#;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let fresh = [("Stripe-Signature", stripe_signature(now, body))];
    assert_eq!(
        post(&l, "/hooks/stripe", &fresh, body).await.signature,
        Some(Signature::Valid)
    );
    let old = [("Stripe-Signature", stripe_signature(now - 600, body))];
    assert_eq!(
        post(&l, "/hooks/stripe", &old, body).await.signature,
        Some(Signature::Expired)
    );
}

#[tokio::test]
async fn generic_base64_hmac_is_valid() {
    let l = start().await;
    let body = "order=42&total=9.99";
    let mac = Algorithm::Sha256.hmac(SHOP_SECRET.as_bytes(), &[body.as_bytes()]);
    let signed = [("X-Signature", STANDARD.encode(mac))];
    let capture = post(&l, "/hooks/shop/orders", &signed, body).await;
    assert_eq!(capture.signature, Some(Signature::Valid));
    let wrong = [("X-Signature", STANDARD.encode(b"nope"))];
    let capture = post(&l, "/hooks/shop/orders", &wrong, body).await;
    assert_eq!(capture.signature, Some(Signature::Invalid));
}

/// Canary: verifier keys never appear in captures or event-log lines, and
/// a sender that echoes one is redacted by the verifiers' redactor.
#[tokio::test]
async fn secrets_never_appear_in_capture_or_log_output() {
    let mut l = start().await;
    let body = r#"{"action":"opened"}"#;
    post(
        &l,
        "/hooks/github",
        &[("X-Hub-Signature-256", github_signature(body))],
        body,
    )
    .await;
    let echoed = format!("leak={SHOP_SECRET}");
    post(
        &l,
        &format!("/hooks/shop/x?{echoed}"),
        &[("X-Signature", "x".into())],
        "",
    )
    .await;

    let redactor = verifiers().redactor();
    let mut lines = Vec::new();
    while let Ok(event) = l.events.try_recv() {
        lines.push(redactor.text(&event.to_string()).into_owned());
    }
    let captures: Vec<String> = l
        .store
        .snapshot()
        .iter()
        .map(|c| format!("{c:?}"))
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].ends_with("signature: valid"), "{lines:?}");
    assert!(
        lines[1].contains("leak=«redacted:shop_secret»"),
        "{lines:?}"
    );
    for secret in [GH_SECRET, STRIPE_SECRET] {
        for text in lines.iter().chain(&captures) {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(!format!("{:?}", verifiers()).contains(secret));
    }
}
