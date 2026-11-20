//! Workspace plugin trust (plugins spec, "Workspace plugin trust",
//! "Headless plugin trust"; task 4.1, 4.2): the plugin set's hash, the
//! sealed approval in `.rustman/trust.yaml`, and the headless rule.

mod common;

use common::Ws;
use rustman_core::trust::TrustKey;
use rustman_plugin::pin::sha256_hex;
use rustman_plugin::trust::{Trust, UNTRUSTED, approve, check, require};

/// A workspace, and this machine's trust key in a folder of its own.
fn ws() -> (Ws, TrustKey) {
    let ws = Ws::new();
    ws.write(".gitignore", ".rustman/\nkeys/\n");
    let key = TrustKey::in_dir(ws.root().join("keys/this"));
    (ws, key)
}

fn prompt(ws: &Ws, key: &TrustKey) -> rustman_core::plugins::PluginTrustPrompt {
    match check(ws.root(), key.clone()).unwrap() {
        Trust::Untrusted(prompt) => prompt,
        other => panic!("expected a prompt, got {other:?}"),
    }
}

#[test]
fn a_workspace_without_plugins_needs_no_trust() {
    let (ws, key) = ws();
    assert_eq!(check(ws.root(), key.clone()).unwrap(), Trust::NoPlugins);
    assert!(!require(ws.root(), key, false).unwrap());
}

#[test]
fn approving_trusts_exactly_the_set_shown() {
    let (ws, key) = ws();
    ws.write("plugins/a.wasm", b"a");
    ws.declare("a", "plugins/a.wasm", &sha256_hex(b"a"), &["clock"]);
    let first = prompt(&ws, &key);
    assert!(!first.changed);
    assert_eq!(first.plugins[0].name, "a");
    assert_eq!(first.plugins[0].version, "unknown");
    assert_eq!(first.plugins[0].grants, ["read the clock"]);

    // The set changed while the prompt was shown: nothing is recorded.
    ws.write("plugins/b.wasm", b"b");
    ws.declare("b", "plugins/b.wasm", &sha256_hex(b"b"), &[]);
    let error = approve(ws.root(), key.clone(), &first.fingerprint).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed while the trust prompt was shown")
    );
    assert!(!ws.root().join(".rustman/trust.yaml").exists());

    let second = prompt(&ws, &key);
    approve(ws.root(), key.clone(), &second.fingerprint).unwrap();
    assert_eq!(check(ws.root(), key.clone()).unwrap(), Trust::Trusted);
    assert!(require(ws.root(), key.clone(), false).unwrap());

    // Another machine's key does not accept this approval.
    let other = TrustKey::in_dir(ws.root().join("keys/other"));
    assert!(prompt(&ws, &other).changed);
}

#[test]
fn headless_runs_refuse_untrusted_sets_unless_opted_in() {
    let (ws, key) = ws();
    ws.write("plugins/a.wasm", b"a");
    ws.declare("a", "plugins/a.wasm", &sha256_hex(b"a"), &[]);
    let error = require(ws.root(), key.clone(), false).unwrap_err();
    assert_eq!(error.to_string(), UNTRUSTED);
    assert_eq!(error.exit_code(), 2);
    assert!(require(ws.root(), key.clone(), true).unwrap());
    assert!(require(ws.root(), key, false).unwrap());
}

/// Raising a plugin's limits changes the set: trust is asked again.
#[test]
fn raised_limits_ask_for_trust_again() {
    let (ws, key) = ws();
    ws.write("plugins/a.wasm", b"a");
    ws.declare("a", "plugins/a.wasm", &sha256_hex(b"a"), &[]);
    approve(ws.root(), key.clone(), &prompt(&ws, &key).fingerprint).unwrap();
    assert_eq!(check(ws.root(), key.clone()).unwrap(), Trust::Trusted);

    let settings = ws.root().join("rustman.toml");
    let mut text = std::fs::read_to_string(&settings).unwrap();
    text.push_str("limits = { cpu_ms = 10000, memory_mib = 1024 }\n");
    std::fs::write(&settings, text).unwrap();
    assert!(prompt(&ws, &key).changed);
}

/// The pin covers only the `.wasm` file: editing a manifest kept next to
/// it asks for trust again.
#[test]
fn an_edited_sidecar_manifest_asks_for_trust_again() {
    let (ws, key) = ws();
    ws.write("plugins/a.wasm", b"a");
    ws.write("plugins/rustman-plugin.toml", "name = \"a\"\n");
    ws.declare("a", "plugins/a.wasm", &sha256_hex(b"a"), &[]);
    approve(ws.root(), key.clone(), &prompt(&ws, &key).fingerprint).unwrap();
    assert_eq!(check(ws.root(), key.clone()).unwrap(), Trust::Trusted);

    ws.write(
        "plugins/rustman-plugin.toml",
        "name = \"a\"\nextension_points = [\"kind\"]\n",
    );
    assert!(prompt(&ws, &key).changed);
}

/// What a trusted load reads is checked against the approval: a manifest
/// edited after the trust check (here, after `require` passed) is refused
/// rather than loaded with capabilities nobody approved.
#[test]
fn a_set_changed_between_the_check_and_the_load_is_refused() {
    use rustman_plugin::fixtures;
    use rustman_plugin::trust::{CHANGED_WHILE_LOADING, load_trusted};

    let (ws, key) = ws();
    let wasm = fixtures::faker();
    ws.write("plugins/a.wasm", &wasm);
    let manifest = fixtures::manifest("a", &["generator"], &["random"]);
    ws.write("plugins/rustman-plugin.toml", manifest.as_str());
    ws.declare("a", "plugins/a.wasm", &sha256_hex(&wasm), &[]);
    approve(ws.root(), key.clone(), &prompt(&ws, &key).fingerprint).unwrap();
    let block_on = |key: TrustKey| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(load_trusted(ws.root(), key, &Default::default()))
    };
    assert!(require(ws.root(), key.clone(), false).unwrap());
    assert_eq!(block_on(key.clone()).unwrap().plugins.len(), 1);

    assert!(require(ws.root(), key.clone(), false).unwrap());
    let widened = fixtures::manifest("a", &["generator"], &["random", "clock"]);
    ws.write("plugins/rustman-plugin.toml", widened.as_str());
    let error = block_on(key).unwrap_err();
    assert_eq!(error.to_string(), CHANGED_WHILE_LOADING);
}
