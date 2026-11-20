//! The examples in docs/plugins.md (task 2.4): the documented manifest,
//! `rustman.toml` tables and `rustman.lock` load as written, with test
//! components standing in for the real plugins (their pins are swapped in),
//! and the error texts quoted on the page are the engine's.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::Ws;
use rustman_plugin::declare::parse_declarations;
use rustman_plugin::fixtures;
use rustman_plugin::grant::Grant;
use rustman_plugin::limits::Limits;
use rustman_plugin::lock::Lockfile;
use rustman_plugin::pin::sha256_hex;
use rustman_plugin::sandbox::HostContext;
use rustman_plugin::{LoadOptions, Manifest, Plugin, load};

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plugins.md");
    std::fs::read_to_string(path).unwrap()
}

/// The ```toml block whose first line is `# <path>`, without that line.
fn file(doc: &str, path: &str) -> String {
    let header = format!("```toml\n# {path}\n");
    let start = doc
        .find(&header)
        .unwrap_or_else(|| panic!("no block for {path}"))
        + header.len();
    let end = doc[start..].find("```").unwrap() + start;
    doc[start..end].to_owned()
}

/// `text` with the `sha256` of `[plugins.<name>]` replaced by `pin`.
fn repin(text: &str, name: &str, pin: &str) -> String {
    let table = format!("[plugins.{name}]");
    let at = text.find(&table).unwrap();
    let line = text[at..].find("sha256 = \"").unwrap() + at + "sha256 = \"".len();
    format!("{}{pin}{}", &text[..line], &text[line + 64..])
}

#[tokio::test]
async fn the_documented_workspace_loads() {
    let doc = doc();
    let manifest_text = file(&doc, "plugins/hmac/rustman-plugin.toml");
    let manifest = Manifest::parse("hmac", &manifest_text).unwrap();
    assert_eq!(manifest.component.as_deref(), Some("hmac.wasm"));

    let ws = Ws::new();
    // A folder source: the documented manifest next to an auth component.
    let hmac = fixtures::signer();
    ws.write("plugins/hmac/rustman-plugin.toml", &manifest_text);
    ws.write("plugins/hmac/hmac.wasm", &hmac);
    // A URL source, already cached: a generator with its manifest embedded.
    let faker_manifest = fixtures::manifest("faker", &["generator"], &["random"]);
    let faker = fixtures::with_manifest(&fixtures::adversarial(), &faker_manifest);
    let faker_pin = sha256_hex(&faker);
    ws.write(&format!(".rustman/plugins/{faker_pin}.wasm"), &faker);
    let settings = file(&doc, "rustman.toml");
    let settings = repin(&settings, "hmac", &sha256_hex(&hmac));
    let settings = repin(&settings, "faker", &faker_pin);
    ws.write("rustman.toml", &settings);

    let set = load(ws.root(), &LoadOptions::default()).await.unwrap();

    let names: Vec<&str> = set.plugins.iter().map(|p| p.name()).collect();
    assert_eq!(names, ["faker", "hmac"]);
    let hmac_plugin = set.get("hmac").unwrap();
    assert_eq!(
        hmac_plugin.grants,
        [Grant::Clock, Grant::SecretsRead("hmac_key".into())]
    );
    assert_eq!(
        hmac_plugin.declaration.limits,
        Limits {
            cpu_ms: 200,
            memory_mib: 64
        }
    );
    for verified in &set.plugins {
        Plugin::new(verified, Arc::new(HostContext::new(ws.root()))).unwrap();
    }
}

#[test]
fn the_documented_lockfile_parses() {
    let doc = doc();
    let lock = Lockfile::parse(&file(&doc, "rustman.lock")).unwrap();
    let declared = parse_declarations(&file(&doc, "rustman.toml")).unwrap();
    assert_eq!(lock.plugins.len(), declared.len());
    for declaration in &declared {
        let locked = lock.get(&declaration.name).unwrap();
        assert_eq!(locked.source, declaration.source.as_str());
        assert_eq!(locked.sha256, declaration.sha256);
        assert_eq!(locked.api, 1);
    }
    assert_eq!(Lockfile::parse(&lock.render()).unwrap(), lock);
}

#[test]
fn the_quoted_errors_are_the_engines() {
    let doc = doc();
    let quoted = [
        "plugin hmac: rustman-plugin.toml: missing field 'version'",
        "plugin hmac: unknown extension point 'telemetry'",
        "plugin sources must use https",
        "plugin hmac: wildcard grants are not allowed",
        "plugin hmac: grant 'fs:read:../' escapes the workspace",
        "plugin hmac: grant 'env:HOME' not requested by the plugin",
        "plugin <name>: memory limit exceeds maximum (1024 MiB)",
        "plugin <name>: execution time limit exceeded",
        "plugin next targets plugin API 2; this rustman supports 1",
        "capability_denied: net evil.example.net",
        "plugin <name>: kind 'http' is reserved",
        "plugin <name>: auth plugins may not change method, host or body",
        r#"plugin soapy: kind 'soap': /: "action" is a required property"#,
    ];
    for text in quoted {
        assert!(doc.contains(text), "{text}");
    }
    let manifest = file(&doc, "plugins/hmac/rustman-plugin.toml");
    let without_version: String = manifest
        .lines()
        .filter(|l| !l.starts_with("version"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        Manifest::parse("hmac", &without_version)
            .unwrap_err()
            .to_string(),
        quoted[0]
    );
    let telemetry = manifest.replace("[\"auth\"]", "[\"telemetry\"]");
    assert_eq!(
        Manifest::parse("hmac", &telemetry).unwrap_err().to_string(),
        quoted[1]
    );
    let manifest = Manifest::parse("hmac", &manifest).unwrap();
    let grants = |g: &str| {
        rustman_plugin::grant::parse_grants("hmac", &[g.to_owned()], &manifest.capabilities)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(grants("net:*"), quoted[3]);
    assert_eq!(grants("env:HOME"), quoted[5]);
}
