//! Transformers, request kinds and importers in real runs
//! (add-plugin-system tasks 5.4 to 5.6): a response decrypted before its
//! assertions, with history keeping both bodies; a request transformer
//! that sees placeholders; a plugin kind that loads, is validated and
//! runs; a reserved kind; and `rustman import <format> <src>` through an
//! importer plugin, writing nothing when its output is invalid.

#![cfg(feature = "plugins")]

mod common;

use std::path::Path;

use common::{Server, request, rustman, workspace, write};
use predicates::prelude::*;
use rustman_plugin::fixtures;
use rustman_plugin::pin::sha256_hex as sha256;

/// Write `wasm` (with a manifest for `point` and `caps`) as
/// `plugins/<name>.wasm` and declare it with `grants`.
fn add_plugin(root: &Path, name: &str, wasm: &[u8], point: &str, caps: &[&str], grants: &[&str]) {
    let manifest = fixtures::manifest(name, &[point], caps);
    let wasm = fixtures::with_manifest(wasm, &manifest);
    write_bytes(root, &format!("plugins/{name}.wasm"), &wasm);
    let grants: Vec<String> = grants.iter().map(|g| format!("\"{g}\"")).collect();
    let table = format!(
        "\n[plugins.{name}]\nsource = \"plugins/{name}.wasm\"\nsha256 = \"{}\"\ngrants = [{}]\n",
        sha256(&wasm),
        grants.join(", ")
    );
    let settings = root.join("rustman.toml");
    let mut text = std::fs::read_to_string(&settings).unwrap();
    text.push_str(&table);
    std::fs::write(settings, text).unwrap();
}

fn write_bytes(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn xor(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|b| b ^ 1).collect()
}

fn history_text(root: &Path) -> String {
    let mut text = String::new();
    let Ok(entries) = std::fs::read_dir(root.join(".rustman/history")) else {
        return text;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            text.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
    text
}

/// A server of fixed files, on its own runtime.
struct Files {
    server: rustman_test_support::FileServer,
    _runtime: tokio::runtime::Runtime,
}

impl Files {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(rustman_test_support::FileServer::http());
        Self {
            server,
            _runtime: runtime,
        }
    }
}

/// "Decrypt a response before assertions": the assertions see the
/// decrypted body, and history stores both bodies.
#[test]
fn decrypt_a_response_before_assertions() {
    let server = Server::http();
    let files = Files::new();
    let plain = br#"{"total":42,"currency":"EUR"}"#;
    files.server.serve("/order", xor(plain));
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "acme",
        &fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    );
    let extra = "transform: {response: [acme/decrypt]}\n\
        assertions:\n  - json: {path: $.total, equals: 42}\n";
    write(
        dir.path(),
        "order.rman.yaml",
        &request("order", &files.server.url("/order"), extra),
    );
    let output = rustman(dir.path())
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .args(["--reporter", "json:out/report.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let history = history_text(dir.path());
    let entry: serde_json::Value = serde_json::from_str(&history).unwrap();
    let record = &entry["record"];
    assert_eq!(
        record["exchange"]["response"]["body"],
        std::str::from_utf8(plain).unwrap()
    );
    let encrypted = String::from_utf8(xor(plain)).unwrap();
    assert_eq!(
        record["transform"]["original_body"],
        encrypted.as_str(),
        "{entry}"
    );
    assert_eq!(record["transform"]["response"][0], "acme/decrypt");
}

/// A failing response transformer is a request error (exit code 3).
#[test]
fn a_transformer_failure_is_a_request_error() {
    let server = Server::http();
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "acme",
        &fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    );
    let extra = "transform: {response: [ghost/decrypt]}\n";
    write(
        dir.path(),
        "order.rman.yaml",
        &request("order", "{{base_url}}/echo", extra),
    );
    rustman(dir.path())
        .args(["send", "order", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(3);
}

/// "Plugin without a grant": a request transformer holding no secret
/// grant sees, and so sends, the placeholder instead of the secret.
#[test]
fn transformers_without_a_grant_see_placeholders() {
    const TOKEN: &str = "tok-CANARY-0f3b7d";
    let server = Server::http();
    let dir = workspace(&server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [api_token]\n",
        server.url("")
    );
    write(dir.path(), "environments/dev.yaml", &env);
    rustman(dir.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(TOKEN)
        .assert()
        .success();
    add_plugin(
        dir.path(),
        "acme",
        &fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    );
    // Encrypting twice gives back what the transformer saw.
    let extra = "method: POST\nbody:\n  type: text\n  content: 'token={{api_token}}'\n\
        transform: {request: [acme/encrypt, acme/encrypt]}\n";
    write(
        dir.path(),
        "token.rman.yaml",
        &request("token", "{{base_url}}/echo", extra),
    );
    let output = rustman(dir.path())
        .args([
            "send",
            "token",
            "--env",
            "dev",
            "--trust-plugins",
            "--body-only",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let echo: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(echo["body"], "token=«redacted:api_token»");
    assert!(!history_text(dir.path()).contains(TOKEN));
}

const SOAP: &str = "version: 1\nkind: soap\nname: get-order\n\
    endpoint: '{{base_url}}/soap'\naction: GetOrder\n";

/// "Plugin kind loads": `kind: soap` files load, are validated against the
/// kind's schema and run through the plugin.
#[test]
fn plugin_kind_loads_and_runs() {
    let server = Server::http();
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "soapy",
        &fixtures::kind_plugin("soap"),
        "kind",
        &[],
        &[],
    );
    write(dir.path(), "order.rman.yaml", SOAP);
    let output = rustman(dir.path())
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .args(["--reporter", "json:out/report.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report = std::fs::read_to_string(dir.path().join("out/report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    let request = &report["requests"][0];
    assert_eq!(request["status"], "passed", "{report}");
    let kind = &request["plugin_kind"];
    assert_eq!(kind["kind"], "soap");
    assert_eq!(kind["status"], 200);
    let endpoint = server.url("/soap");
    assert!(
        kind["summary"].as_str().unwrap().contains(&endpoint),
        "{kind}"
    );
    assert!(history_text(dir.path()).contains("plugin_kind"));
    // `send` prints the response body the plugin returned.
    rustman(dir.path())
        .args(["send", "get-order", "--env", "dev", "--trust-plugins"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"action\":\"GetOrder\""));
}

/// Commands that do not run requests load the workspace with plugin kinds
/// too: a `kind: soap` file stops none of `env`, `secret`, `history`,
/// `snapshot` or `report render --format <plugin>/<format>`.
#[test]
fn workspace_commands_load_plugin_kinds() {
    let server = Server::http();
    let dir = workspace(&server);
    let root = dir.path();
    add_plugin(
        root,
        "soapy",
        &fixtures::kind_plugin("soap"),
        "kind",
        &[],
        &[],
    );
    add_plugin(
        root,
        "acme",
        &fixtures::report_renderer(),
        "report",
        &[],
        &[],
    );
    write(root, "order.rman.yaml", SOAP);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [hmac_key]\n",
        server.url("")
    );
    write(root, "environments/dev.yaml", &env);
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .args(["--reporter", "json:out/report.json"])
        .assert()
        .code(0);
    let commands: [&[&str]; 6] = [
        &["env", "list"],
        &["secret", "set", "hmac_key", "--env", "dev"],
        &["secret", "rm", "hmac_key", "--env", "dev"],
        &["history", "list"],
        &["snapshot", "list"],
        &[
            "report",
            "render",
            "out/report.json",
            "--format",
            "acme/xlsx",
            "-o",
            "r.xlsx",
        ],
    ];
    for args in commands {
        let output = rustman(root).args(args).write_stdin("v").output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    }
    assert!(root.join("r.xlsx").is_file());
}

/// A plugin kind request whose fields do not match the kind's schema
/// stops the run before anything is sent (exit code 2).
#[test]
fn plugin_kind_fields_are_validated() {
    let server = Server::http();
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "soapy",
        &fixtures::kind_plugin("soap"),
        "kind",
        &[],
        &[],
    );
    write(
        dir.path(),
        "order.rman.yaml",
        "version: 1\nkind: soap\nname: get-order\nendpoint: 1\n",
    );
    rustman(dir.path())
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("plugin soapy: kind 'soap': "));
}

/// A kind no loaded plugin registers is still an unknown kind.
#[test]
fn unknown_kinds_are_refused_with_plugins_declared() {
    let server = Server::http();
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "soapy",
        &fixtures::kind_plugin("soap"),
        "kind",
        &[],
        &[],
    );
    write(
        dir.path(),
        "order.rman.yaml",
        "version: 1\nkind: echo\nname: e\n",
    );
    rustman(dir.path())
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unknown request kind 'echo' (expected one of: http, graphql, jsonrpc, grpc, mcp, ws, sse, kafka, mqtt, amqp, evm-call, evm-tx, soap)",
        ));
}

/// "Built-in kind collision".
#[test]
fn builtin_kind_collision() {
    let server = Server::http();
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "sneaky",
        &fixtures::kind_plugin("http"),
        "kind",
        &[],
        &[],
    );
    write(
        dir.path(),
        "one.rman.yaml",
        &request("one", "{{base_url}}/echo", ""),
    );
    rustman(dir.path())
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plugin sneaky: kind 'http' is reserved",
        ));
}

/// A workspace with the `copy` importer granted `fs:read:src`, and the
/// source `src/source.txt` holding `content`.
fn import_workspace(content: &str) -> tempfile::TempDir {
    let server = Server::http();
    let dir = workspace(&server);
    let caps = ["fs:read"];
    add_plugin(
        dir.path(),
        "copier",
        &fixtures::copy_importer(),
        "importer",
        &caps,
        &["fs:read:src"],
    );
    write(dir.path(), "src/source.txt", content);
    dir
}

/// "Plugin importer": the returned request is validated and written by the
/// host as a `.rman.yaml` file.
#[test]
fn plugin_importer() {
    let content = "version: 1\nkind: http\nname: imported\nurl: https://api.example.com/\n";
    let dir = import_workspace(content);
    rustman(dir.path())
        .args([
            "import",
            "copy",
            "src/source.txt",
            "--into",
            "imported",
            "--trust-plugins",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created imported/imported.rman.yaml",
        ));
    let written = std::fs::read_to_string(dir.path().join("imported/imported.rman.yaml")).unwrap();
    assert_eq!(written, content);
    let path = Path::new("imported/imported.rman.yaml");
    let loaded = rustman_core::model::KindRegistry::default().load(path, &written);
    assert!(loaded.is_ok(), "{loaded:?}");
}

/// "Invalid output": a request of an unregistered kind writes nothing, and
/// the error names the plugin and the request.
#[test]
fn invalid_importer_output_writes_nothing() {
    let dir = import_workspace("version: 1\nkind: nope\nname: bad\n");
    rustman(dir.path())
        .args([
            "import",
            "copy",
            "src/source.txt",
            "--into",
            "imported",
            "--trust-plugins",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plugin copier: imported.rman.yaml: unknown request kind 'nope'",
        ));
    assert!(!dir.path().join("imported").exists());
}

/// A source outside the workspace is refused, and the error names the
/// source, not `--into`.
#[test]
fn import_sources_outside_the_workspace_are_refused() {
    let dir = import_workspace("version: 1\nkind: http\nname: a\nurl: https://a.example/\n");
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("outside.txt");
    std::fs::write(&source, "x").unwrap();
    let output = rustman(dir.path())
        .args(["import", "copy"])
        .arg(&source)
        .arg("--trust-plugins")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be inside the workspace"), "{stderr}");
    assert!(stderr.contains("outside.txt"), "{stderr}");
    assert!(!stderr.contains("--into"), "{stderr}");
}

/// Importers read only what their `fs:read` grants cover, and an untrusted
/// plugin set imports nothing.
#[test]
fn importers_need_grants_and_trust() {
    let dir = import_workspace("version: 1\nkind: http\nname: a\nurl: https://a.example/\n");
    write(dir.path(), "other/source.txt", "x");
    rustman(dir.path())
        .args(["import", "copy", "other/source.txt", "--trust-plugins"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plugin copier: capability_denied: fs other/source.txt",
        ));
    let fresh = import_workspace("version: 1\nkind: http\nname: a\nurl: https://a.example/\n");
    rustman(fresh.path())
        .args(["import", "copy", "src/source.txt"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "workspace plugins are not trusted",
        ));
}
