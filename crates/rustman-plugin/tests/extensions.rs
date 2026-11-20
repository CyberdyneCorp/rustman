//! The extension points served by a loaded plugin set (add-plugin-system
//! section 5): an HMAC signer, a host-rewriting and a trapping auth
//! plugin, a `faker` generator and a `price-consistent` assertion, called
//! through [`PluginRuntime`] as the runner calls them.

mod common;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{Ws, verified};
use hmac::{Hmac, KeyInit, Mac};
use rustman_core::Secret;
use rustman_core::plugins::{Extensions, PluginExchange, PluginVerdict, SignRequest};
use rustman_plugin::PluginRuntime;
use rustman_plugin::fixtures;
use rustman_plugin::sandbox::{HostContext, HostServices, HttpRequest, HttpResponse};
use rustman_plugin::workspace::PluginSet;

const KEY: &str = "hmac-key-0123456789";
const NOW: u64 = 1_767_225_600; // 2026-01-01T00:00:00Z

/// A fixed clock and a counting random source.
struct Fake;

impl HostServices for Fake {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW)
    }
    fn fill_random(&self, bytes: &mut [u8]) {
        rand_bytes(bytes);
    }
    fn env(&self, _: &str) -> Option<String> {
        None
    }
    fn fetch(
        &self,
        _: &str,
        _: HttpRequest,
    ) -> Result<HttpResponse, rustman_plugin::plugin::HostError> {
        unreachable!()
    }
}

fn rand_bytes(bytes: &mut [u8]) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static NEXT: AtomicU8 = AtomicU8::new(1);
    for byte in bytes {
        *byte = NEXT.fetch_add(1, Ordering::SeqCst);
    }
}

/// A runtime with one plugin, `name`, holding `grants`.
fn runtime(
    name: &str,
    wasm: Vec<u8>,
    points: &[&str],
    caps: &[&str],
    grants: &[&str],
) -> PluginRuntime {
    let ws = Ws::new();
    let manifest = fixtures::manifest(name, points, caps);
    let set = PluginSet {
        plugins: vec![verified(name, wasm, &manifest, grants)],
        ..PluginSet::default()
    };
    let context = HostContext::new(ws.root())
        .with_secrets(vec![Secret::new("hmac_key", KEY.to_owned())])
        .with_services(Arc::new(Fake));
    PluginRuntime::new(&set, context).unwrap()
}

fn sign_request() -> SignRequest {
    SignRequest {
        method: "POST".into(),
        url: "http://127.0.0.1:8080/orders?x=1".into(),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: b"{}".to_vec(),
        params: vec![("key_var".into(), "hmac_key".into())],
    }
}

fn expected_signature(message: &str) -> String {
    let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn the_hmac_plugin_signs_with_its_granted_secret_and_the_clock() {
    let hmac = runtime(
        "hmac",
        fixtures::hmac_signer(),
        &["auth"],
        &["secrets:read", "clock"],
        &["secrets:read:hmac_key", "clock"],
    );
    let output = hmac.sign("hmac/v1", &sign_request()).unwrap();
    let message = format!("{NOW}\nPOST\nhttp://127.0.0.1:8080/orders?x=1");
    assert_eq!(
        output.set_headers,
        [
            ("X-Signature".to_owned(), expected_signature(&message)),
            ("X-Timestamp".to_owned(), NOW.to_string()),
        ]
    );
    assert!(output.add_query.is_empty());
    assert_eq!(output.url, None);
}

#[test]
fn the_hmac_plugin_handles_long_messages_and_keys() {
    // Messages over one SHA-256 block exercise the multi-block path.
    let hmac = runtime(
        "hmac",
        fixtures::hmac_signer(),
        &["auth"],
        &["secrets:read", "clock"],
        &["secrets:read:hmac_key", "clock"],
    );
    let url = format!("http://127.0.0.1:8080/{}", "a".repeat(300));
    let request = SignRequest {
        url: url.clone(),
        ..sign_request()
    };
    let output = hmac.sign("hmac/v1", &request).unwrap();
    let message = format!("{NOW}\nPOST\n{url}");
    assert_eq!(output.set_headers[0].1, expected_signature(&message));
}

#[test]
fn without_its_grants_the_hmac_plugin_cannot_sign() {
    let hmac = runtime(
        "hmac",
        fixtures::hmac_signer(),
        &["auth"],
        &["secrets:read", "clock"],
        &["clock"],
    );
    let error = hmac.sign("hmac/v1", &sign_request()).unwrap_err();
    assert_eq!(error, "plugin hmac: secrets hmac_key");
}

#[test]
fn a_trapping_auth_plugin_is_a_plugin_error() {
    let auth = runtime("aws-sigv4", fixtures::trapping_auth(), &["auth"], &[], &[]);
    let error = auth.sign("aws-sigv4/v1", &sign_request()).unwrap_err();
    assert!(error.starts_with("plugin aws-sigv4 trapped: "), "{error}");
}

#[test]
fn the_faker_generator_runs_for_each_call() {
    let faker = runtime(
        "faker",
        fixtures::faker(),
        &["generator"],
        &["random"],
        &["random"],
    );
    let first = faker.generate("faker", "email", &[]).unwrap();
    let second = faker.generate("faker", "email", &[]).unwrap();
    assert_ne!(first, second);
    for email in [&first, &second] {
        assert!(
            email.starts_with("user-") && email.ends_with("@example.test"),
            "{email}"
        );
        assert_eq!(email.len(), "user-".len() + 8 + "@example.test".len());
    }
    let denied = runtime("faker", fixtures::faker(), &["generator"], &["random"], &[]);
    let error = denied.generate("faker", "email", &[]).unwrap_err();
    assert_eq!(error, "plugin faker: random");
}

fn exchange(body: &str) -> PluginExchange {
    PluginExchange {
        method: "GET".into(),
        url: "http://127.0.0.1/order".into(),
        status: 200,
        response_body: body.as_bytes().to_vec(),
        ..PluginExchange::default()
    }
}

#[test]
fn the_price_check_compares_the_total_with_the_line_amounts() {
    let shop = runtime("shop", fixtures::price_check(), &["assertion"], &[], &[]);
    let wrong = r#"{"total": 99.0, "lines": [{"amount": 49.0}, {"amount": 49.5}]}"#;
    assert_eq!(
        shop.check("shop/price-consistent", &exchange(wrong), "{}"),
        PluginVerdict::Fail("total 99.0 != sum 98.5".into())
    );
    let right = r#"{"total": 98.5, "lines": [{"amount": 49.0}, {"amount": 49.5}]}"#;
    assert_eq!(
        shop.check("shop/price-consistent", &exchange(right), "{}"),
        PluginVerdict::Pass
    );
    assert_eq!(
        shop.check("shop/price-consistent", &exchange("{}"), "{}"),
        PluginVerdict::Error("the response has no total".into())
    );
}

#[test]
fn unknown_plugins_and_names_are_errors() {
    let shop = runtime("shop", fixtures::price_check(), &["assertion"], &[], &[]);
    assert_eq!(
        shop.check("other/fn", &exchange("{}"), "{}"),
        PluginVerdict::Error("plugin other is not loaded".into())
    );
    assert_eq!(
        shop.sign("shop/v1", &sign_request()).unwrap_err(),
        "plugin shop: does not declare extension point 'auth'"
    );
}

#[test]
fn a_report_renderer_returns_the_bytes_to_write() {
    let acme = runtime("acme", fixtures::report_renderer(), &["report"], &[], &[]);
    let report = r#"{"schema":"rustman.report/v1"}"#;
    let bytes = acme.render("acme/xlsx", report).unwrap();
    let mut expected = fixtures::REPORT_PREFIX.to_vec();
    expected.extend_from_slice(report.as_bytes());
    assert_eq!(bytes, expected);
}
