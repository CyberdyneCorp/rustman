//! Resolving `evm-call` files: explorer API keys are resolved only for a
//! call that may fetch its ABI from an explorer.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustman_core::Workspace;
use rustman_core::vars::{Resolver, SecretLookup, resolve_evm_call};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

/// Counts every lookup of the explorer key.
#[derive(Default)]
struct Counting(AtomicUsize);

impl SecretLookup for Counting {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        if name == "etherscan_key" {
            self.0.fetch_add(1, Ordering::SeqCst);
            return Ok(Some("explorer-k3y".to_owned()));
        }
        Ok(None)
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The number of key lookups and explorer keys when resolving a call
/// with `abi` under `[evm] allow_explorer_fetch = allow`.
fn resolve(abi: &str, allow: bool) -> (usize, usize) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n\n[evm]\nallow_explorer_fetch = {allow}\n\n[evm.explorers.1]\napi_key = \"{{{{etherscan_key}}}}\"\n"
    );
    write(dir.path(), "rustman.toml", &toml);
    write(
        dir.path(),
        "environments/prod.yaml",
        "variables:\n  rpc: http://node.local\nsecrets: [etherscan_key]\n",
    );
    let call = format!(
        "version: 1\nkind: evm-call\nname: c\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: {abi}\nfunction: totalSupply\n"
    );
    write(dir.path(), "c.rman.yaml", &call);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("c")[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = Counting::default();
    let resolver = Resolver::for_request(&ws.settings, request, env, &store);
    let resolved = resolve_evm_call(request, &resolver, &ws.settings.evm).unwrap();
    (store.0.load(Ordering::SeqCst), resolved.explorer_keys.len())
}

#[test]
fn explorer_keys_are_resolved_only_for_an_allowed_explorer_fetch() {
    // Regression: every evm-call asked the secret provider for every
    // explorer key, even with explorer fetch disabled or unused.
    assert_eq!(resolve("erc20", true), (0, 0));
    assert_eq!(resolve("{explorer: true}", false), (0, 0));
    let (lookups, keys) = resolve("{explorer: true}", true);
    assert!(lookups >= 1);
    assert_eq!(keys, 1);
}

#[test]
fn an_unresolved_ens_rpc_keeps_its_reason() {
    // Regression: an `[evm] ens_rpc` naming an unknown variable was
    // dropped, and a call needing ENS said "set [evm] ens_rpc" instead.
    let dir = tempfile::tempdir().unwrap();
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n\n[evm]\nens_rpc = \"{{mainnet_rpc}}\"\n";
    write(dir.path(), "rustman.toml", toml);
    write(
        dir.path(),
        "environments/prod.yaml",
        "variables:\n  rpc: http://node.local\n",
    );
    let call = format!(
        "version: 1\nkind: evm-call\nname: c\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: totalSupply\n"
    );
    write(dir.path(), "c.rman.yaml", &call);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("c")[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = Counting::default();
    let resolver = Resolver::for_request(&ws.settings, request, env, &store);
    let resolved = resolve_evm_call(request, &resolver, &ws.settings.evm).unwrap();
    let Some(Err(reason)) = resolved.ens_rpc else {
        panic!("ens_rpc should carry why it did not resolve");
    };
    assert!(
        reason.starts_with("[evm] ens_rpc:") && reason.contains("mainnet_rpc"),
        "{reason}"
    );
}
