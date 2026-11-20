//! The capability sandbox (plugins spec, "Capability sandbox"; task 3.2):
//! every host import refuses a call without its grant, with no side
//! effect, and no ambient WASI capability exists.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use common::{Ws, plugin, verified};
use rustman_core::Secret;
use rustman_plugin::fixtures;
use rustman_plugin::plugin::{Header, HostError};
use rustman_plugin::sandbox::{HostContext, HostServices, HttpRequest, HttpResponse};

/// Services that count every use: a refused call must use none.
#[derive(Default)]
struct Counting {
    uses: AtomicUsize,
}

impl HostServices for Counting {
    fn now(&self) -> SystemTime {
        self.uses.fetch_add(1, Ordering::SeqCst);
        SystemTime::UNIX_EPOCH
    }

    fn fill_random(&self, _bytes: &mut [u8]) {
        self.uses.fetch_add(1, Ordering::SeqCst);
    }

    fn env(&self, _name: &str) -> Option<String> {
        self.uses.fetch_add(1, Ordering::SeqCst);
        Some("/home/someone".to_owned())
    }

    fn fetch(&self, _plugin: &str, _request: HttpRequest) -> Result<HttpResponse, HostError> {
        self.uses.fetch_add(1, Ordering::SeqCst);
        Ok(HttpResponse {
            status: 200,
            headers: vec![Header {
                name: "x".into(),
                value: "y".into(),
            }],
            body: Vec::new(),
        })
    }
}

const EVERYTHING: [&str; 6] = ["clock", "random", "env", "fs:read", "net", "secrets:read"];

#[test]
fn every_denied_operation_returns_capability_denied_without_side_effects() {
    let ws = Ws::new();
    ws.write("secret.txt", "top secret file");
    let services = Arc::new(Counting::default());
    let context = HostContext::new(ws.root())
        .with_secrets(vec![Secret::new("api_token", "tok-123456789".to_owned())])
        .with_services(services.clone());
    let manifest = fixtures::manifest("adversary", &["generator"], &EVERYTHING);
    let verified = verified("adversary", fixtures::adversarial(), &manifest, &[]);
    let plugin = plugin(&verified, context);

    let out = plugin.generate("probe", &[]).unwrap();

    // Seven calls, each `err(capability-denied(..))`.
    assert_eq!(out, "10101010101010");
    assert_eq!(
        plugin.take_denials(),
        [
            "capability_denied: env HOME",
            "capability_denied: fs",
            "capability_denied: fs",
            "capability_denied: clock",
            "capability_denied: random",
            "capability_denied: net evil.example.net",
            "capability_denied: secrets api_token",
        ]
    );
    assert_eq!(
        services.uses.load(Ordering::SeqCst),
        0,
        "a refused call used a service"
    );
}

#[test]
fn grants_open_exactly_their_capability() {
    let ws = Ws::new();
    ws.write("secret.txt", "top secret file");
    let services = Arc::new(Counting::default());
    let context = HostContext::new(ws.root())
        .with_secrets(vec![Secret::new("api_token", "tok-123456789".to_owned())])
        .with_services(services.clone());
    let manifest = fixtures::manifest("adversary", &["generator"], &EVERYTHING);
    let grants = [
        "env:HOME",
        "fs:read:.",
        "clock",
        "random",
        "secrets:read:api_token",
    ];
    let verified = verified("adversary", fixtures::adversarial(), &manifest, &grants);
    let plugin = plugin(&verified, context);

    let out = plugin.generate("probe", &[]).unwrap();

    // Everything but the network succeeds (`0`); the net grant is missing.
    let tags: Vec<&str> = (0..7).map(|i| &out[2 * i..2 * i + 1]).collect();
    assert_eq!(tags, ["0", "0", "0", "0", "0", "1", "0"], "{out}");
    assert_eq!(
        plugin.take_denials(),
        ["capability_denied: net evil.example.net"]
    );
    // env, clock and random used their service once each.
    assert_eq!(services.uses.load(Ordering::SeqCst), 3);
}

#[test]
fn ungranted_file_read() {
    let ws = Ws::new();
    ws.write("secret.txt", "top secret file");
    let manifest = fixtures::manifest("adversary", &["generator"], &["fs:read"]);
    let verified = verified(
        "adversary",
        fixtures::adversarial(),
        &manifest,
        &["fs:read:data"],
    );
    let plugin = plugin(&verified, HostContext::new(ws.root()));
    plugin.generate("probe", &[]).unwrap();
    let denials = plugin.take_denials();
    assert_eq!(denials[1], "capability_denied: fs");
    assert_eq!(denials[2], "capability_denied: fs");
}

#[test]
fn wasi_imports_are_never_linked() {
    let manifest = fixtures::manifest("wasi", &["generator"], &[]);
    let verified = verified("wasi", fixtures::wasi_importer(), &manifest, &[]);
    let err = rustman_plugin::Plugin::new(&verified, Arc::new(HostContext::new("."))).unwrap_err();
    let message = err.to_string();
    assert!(message.starts_with("plugin wasi: "), "{message}");
    assert!(message.contains("wasi:cli/environment"), "{message}");
    assert!(message.contains("no WASI"), "{message}");
}

#[test]
fn declared_extension_points_must_be_exported() {
    let manifest = fixtures::manifest("probe", &["generator", "auth"], &[]);
    let verified = verified("probe", fixtures::adversarial(), &manifest, &[]);
    let err = rustman_plugin::Plugin::new(&verified, Arc::new(HostContext::new("."))).unwrap_err();
    assert!(
        err.to_string().starts_with(
            "plugin probe: declares extension point 'auth' but does not export rustman:plugin/auth-scheme"
        ),
        "{err}"
    );
    let not_wasm = verified_bytes(b"not wasm".to_vec());
    let err = rustman_plugin::Plugin::new(&not_wasm, Arc::new(HostContext::new("."))).unwrap_err();
    assert!(err.to_string().contains("not a valid component"), "{err}");
}

fn verified_bytes(wasm: Vec<u8>) -> rustman_plugin::VerifiedPlugin {
    let manifest = fixtures::manifest("junk", &["generator"], &[]);
    verified("junk", wasm, &manifest, &[])
}

#[test]
fn calls_of_undeclared_extension_points_fail() {
    let manifest = fixtures::manifest("probe", &["generator"], &[]);
    let verified = verified("probe", fixtures::adversarial(), &manifest, &[]);
    let plugin = plugin(&verified, HostContext::new("."));
    assert_eq!(plugin.generator_names().unwrap(), ["probe"]);
    let err = plugin.auth_schemes().unwrap_err();
    assert_eq!(
        err.to_string(),
        "plugin probe: does not declare extension point 'auth'"
    );
}
