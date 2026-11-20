//! `rustman plugin add|list|remove|verify|api|test|new` (add-plugin-system
//! section 6): "Add without grants" and "Plain HTTP refused" over a local
//! HTTPS server, "Verify in CI" (exit codes 0 and 2), the trust state in
//! `plugin list`, and the host test harness's "Deterministic signing test"
//! with fake clock, randomness, network and secrets, its redaction and its
//! exit codes.

#![cfg(feature = "plugins")]

mod common;

use std::path::Path;

use common::{rustman, write};
use hmac::{Hmac, KeyInit, Mac};
use predicates::prelude::*;
use rustman_plugin::fixtures;
use rustman_plugin::pin::sha256_hex as sha256;
use rustman_test_support::certs::TestCerts;
use rustman_test_support::files::FileServer;

/// A server of fixed files over HTTPS (the test CA's `localhost`), on its
/// own runtime.
struct Files {
    server: FileServer,
    certs: TestCerts,
    _runtime: tokio::runtime::Runtime,
}

impl Files {
    fn https() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let certs = TestCerts::generate();
        let server = runtime.block_on(FileServer::https(&certs));
        Self {
            server,
            certs,
            _runtime: runtime,
        }
    }
}

/// An empty workspace.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

/// The `hmac` auth component, its manifest embedded.
fn hmac_component() -> Vec<u8> {
    let manifest = fixtures::manifest("hmac", &["auth"], &["clock", "secrets:read"]);
    fixtures::with_manifest(&fixtures::hmac_signer(), &manifest)
}

/// "Add without grants": a URL source is downloaded, its manifest and
/// requested capabilities printed, and it is declared with no grants.
#[test]
fn add_without_grants() {
    let files = Files::https();
    let wasm = hmac_component();
    files.server.serve("/hmac-0.2.0.wasm", wasm.clone());
    let dir = workspace();
    let root = dir.path();
    write(root, "ca.pem", &files.certs.ca_pem);
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    write(
        root,
        "rustman.toml",
        &format!("{settings}\n[http]\nca_bundle = \"ca.pem\"\n"),
    );
    let url = files.server.url("/hmac-0.2.0.wasm");
    rustman(root)
        .args(["plugin", "add", &url])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("plugin hmac 0.1.0 (API 1)"))
        .stdout(predicate::str::contains("extension points: auth"))
        .stdout(predicate::str::contains(
            "requested capabilities: clock, secrets:read",
        ));
    let pin = sha256(&wasm);
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    assert!(
        settings.ends_with(&format!(
            "[plugins.hmac]\nsource = \"{url}\"\nsha256 = \"{pin}\"\ngrants = []\n"
        )),
        "{settings}"
    );
    let lock = std::fs::read_to_string(root.join("rustman.lock")).unwrap();
    assert!(lock.contains(&format!("sha256 = \"{pin}\"")), "{lock}");
    assert!(lock.contains("version = \"0.1.0\""), "{lock}");
    let cached = root.join(format!(".rustman/plugins/{pin}.wasm"));
    assert_eq!(std::fs::read(cached).unwrap(), wasm);
    assert_eq!(files.server.hits("/hmac-0.2.0.wasm"), 1);

    // No silent upgrade: verify re-hashes the cached build, nothing is
    // downloaded again, and a second add of the same name is refused.
    files
        .server
        .serve("/hmac-0.2.0.wasm", b"a newer build".to_vec());
    rustman(root)
        .args(["plugin", "verify"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!(
            "ok hmac 0.1.0 sha256 {pin}"
        )));
    rustman(root)
        .args(["plugin", "add", &url])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already declared"));
    assert_eq!(files.server.hits("/hmac-0.2.0.wasm"), 1);
}

/// "Plain HTTP refused", before anything is sent.
#[test]
fn plain_http_refused() {
    let dir = workspace();
    rustman(dir.path())
        .args(["plugin", "add", "http://example.com/p.wasm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("plugin sources must use https"));
    assert!(!dir.path().join("rustman.lock").exists());
}

/// A path source is pinned relative to the workspace, whatever the
/// current directory; a source outside the workspace is refused.
#[test]
fn add_list_and_remove_a_path_source() {
    let dir = workspace();
    let root = dir.path();
    let manifest = fixtures::manifest("faker", &["generator"], &["random"]);
    write(root, "plugins/faker/rustman-plugin.toml", &manifest);
    let wasm = fixtures::faker();
    std::fs::write(root.join("plugins/faker/plugin.wasm"), &wasm).unwrap();
    rustman(&root.join("plugins"))
        .args(["plugin", "add", "./faker", "--name", "fake-data"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("requested capabilities: random"));
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    assert!(
        settings.contains("[plugins.fake-data]\nsource = \"plugins/faker\"\n"),
        "{settings}"
    );
    assert!(settings.contains(&sha256(&wasm)));

    rustman(root)
        .args(["plugin", "list"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "NAME       VERSION  API  SOURCE         GRANTS\nfake-data  0.1.0    1    plugins/faker  none\n",
        ))
        .stdout(predicate::str::contains("plugin set: not trusted"));

    let outside = tempfile::tempdir().unwrap();
    rustman(root)
        .args(["plugin", "add"])
        .arg(outside.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("outside the workspace"));

    rustman(root)
        .args(["plugin", "remove", "fake-data"])
        .assert()
        .code(0);
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    assert_eq!(settings, "[workspace]\nname = \"t\"\nversion = 1\n");
    let lock = std::fs::read_to_string(root.join("rustman.lock")).unwrap();
    assert!(!lock.contains("fake-data"), "{lock}");
    rustman(root)
        .args(["plugin", "list"])
        .assert()
        .code(0)
        .stdout("no plugins declared in rustman.toml\n");
    rustman(root)
        .args(["plugin", "remove", "fake-data"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plugin fake-data: not declared in rustman.toml",
        ));
}

/// A committed plugin that is a symlink to `/dev/zero` is refused at once
/// (exit code 2) by `plugin list` and by a headless run, before trust,
/// instead of being read without end.
#[cfg(unix)]
#[test]
fn a_plugin_linked_to_dev_zero_is_refused_before_trust() {
    let dir = workspace();
    let root = dir.path();
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    let pin = "0".repeat(64);
    write(
        root,
        "rustman.toml",
        &format!(
            "{settings}\n[plugins.evil]\nsource = \"plugins/evil.wasm\"\nsha256 = \"{pin}\"\n"
        ),
    );
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", root.join("plugins/evil.wasm")).unwrap();
    write(
        root,
        "get.rman.yaml",
        "version: 1\nkind: http\nname: Get\nmethod: GET\nurl: http://127.0.0.1:9/\n",
    );
    for args in [&["plugin", "list"][..], &["run", "--trust-plugins"][..]] {
        rustman(root)
            .args(args)
            .timeout(std::time::Duration::from_secs(60))
            .assert()
            .code(2)
            .stderr(
                predicate::str::contains("plugin evil: ")
                    .and(predicate::str::contains("resolves outside the workspace")),
            );
    }
}

/// "Verify in CI": exit 0 when every hash matches, 2 on a mismatch.
#[test]
fn verify_in_ci() {
    let dir = workspace();
    let root = dir.path();
    write(root, "plugins/hmac.wasm", "");
    std::fs::write(root.join("plugins/hmac.wasm"), hmac_component()).unwrap();
    rustman(root)
        .args(["plugin", "add", "plugins/hmac.wasm"])
        .assert()
        .code(0);
    rustman(root).args(["plugin", "verify"]).assert().code(0);

    std::fs::write(root.join("plugins/hmac.wasm"), b"tampered").unwrap();
    rustman(root)
        .args(["plugin", "verify"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "plugin hmac: sha256 mismatch (expected {}, got {})",
            sha256(&hmac_component()),
            sha256(b"tampered")
        )));
}

#[test]
fn api_lists_the_supported_majors() {
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["plugin", "api"])
        .assert()
        .code(0)
        .stdout("rustman:plugin@1.0.0\nsupported plugin API majors: 1\n");
}

#[test]
fn new_scaffolds_a_project() {
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["plugin", "new", "my-auth", "--kind", "auth"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "created the auth plugin my-auth in my-auth",
        ));
    for file in [
        "Cargo.toml",
        "rustman-plugin.toml",
        "src/lib.rs",
        "rustman-plugin-tests.toml",
        "README.md",
    ] {
        assert!(dir.path().join("my-auth").join(file).is_file(), "{file}");
    }
    rustman(dir.path())
        .args(["plugin", "new", "my-auth", "--kind", "auth"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists and is not empty"));
    rustman(dir.path())
        .args(["plugin", "new", "x", "--kind", "telemetry"])
        .assert()
        .code(2);
}

const NOW: u64 = 1_767_225_600;

/// A folder plugin `component` with `manifest` and the test cases `cases`.
fn plugin_folder(root: &Path, component: &[u8], manifest: &str, cases: &str) {
    write(root, "rustman-plugin.toml", manifest);
    std::fs::write(root.join("plugin.wasm"), component).unwrap();
    write(root, "rustman-plugin-tests.toml", cases);
}

fn signature(message: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"test-secret").unwrap();
    mac.update(message.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// "Deterministic signing test": a fixed clock and a fake secret give
/// byte-identical signatures on every run, and exit code 0.
#[test]
fn deterministic_signing_test() {
    let dir = tempfile::tempdir().unwrap();
    let expected = signature(&format!("{NOW}\nPOST\nhttps://api.example.com/orders"));
    let cases = format!(
        r#"
[fakes]
clock = 2026-01-01T00:00:00Z
secrets = {{ hmac_key = "test-secret" }}

[[case]]
name = "signs"
grants = ["clock", "secrets:read:hmac_key"]
sign = {{ scheme = "v1", method = "POST", url = "https://api.example.com/orders", params = {{ key_var = "hmac_key" }} }}
expect = {{ "header.X-Timestamp" = "{NOW}", "header.X-Signature" = "{expected}" }}

[[case]]
name = "no clock without its grant"
grants = ["secrets:read:hmac_key"]
sign = {{ scheme = "v1", url = "https://api.example.com/orders", params = {{ key_var = "hmac_key" }} }}
expect_error = "clock"
"#
    );
    let manifest = fixtures::manifest("hmac", &["auth"], &["clock", "secrets:read"]);
    plugin_folder(dir.path(), &fixtures::hmac_signer(), &manifest, &cases);
    let run = || {
        let output = rustman(dir.path())
            .args(["plugin", "test", "."])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        output.stdout
    };
    let first = run();
    assert_eq!(first, run());
    let text = String::from_utf8(first).unwrap();
    assert!(
        text.starts_with(&format!(
            "plugin hmac 0.1.0\nok   signs\n       header.X-Signature: {expected}\n       header.X-Timestamp: {NOW}\nok   no clock without its grant\n"
        )),
        "{text}"
    );
    assert!(text.ends_with("2 passed, 0 failed\n"), "{text}");
}

/// A failing expectation exits 1 and says what differed; a test file that
/// cannot be read exits 2.
#[test]
fn failing_cases_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = fixtures::manifest("faker", &["generator"], &["random"]);
    let cases = r#"
[fakes]
random_seed = 7

[[case]]
name = "seeded"
grants = ["random"]
generate = { name = "email" }
expect = { value = "nobody@example.test" }

[[case]]
name = "no randomness without its grant"
generate = { name = "email" }
expect_error = "random"
"#;
    plugin_folder(dir.path(), &fixtures::faker(), &manifest, cases);
    let output = rustman(dir.path())
        .args(["plugin", "test"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("FAIL seeded\n"), "{text}");
    assert!(
        text.contains("! expected value = \"nobody@example.test\", got \"user-"),
        "{text}"
    );
    assert!(
        text.contains("ok   no randomness without its grant\n"),
        "{text}"
    );
    // The seed fixes the "random" email: the same on every run.
    let again = rustman(dir.path())
        .args(["plugin", "test"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(again.stdout).unwrap(), text);

    std::fs::write(dir.path().join("rustman-plugin-tests.toml"), "[[case]]\n").unwrap();
    rustman(dir.path())
        .args(["plugin", "test"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("rustman-plugin-tests.toml"));
}

/// The fake network serves canned responses to granted hosts only.
#[test]
fn the_fake_network_answers_granted_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = fixtures::manifest(
        "fetch",
        &["generator"],
        &["net:auth.example.com", "secrets:read"],
    );
    let cases = r#"
[[fakes.http]]
url = "https://auth.example.com/token"
body = "fake-token"

[[case]]
name = "granted"
grants = ["net:auth.example.com"]
generate = { name = "fetch", args = ["https://auth.example.com/token"] }
expect = { value = "fake-token" }

[[case]]
name = "no canned response"
grants = ["net:auth.example.com"]
generate = { name = "fetch", args = ["https://auth.example.com/other"] }
expect_error = "no fake response for GET https://auth.example.com/other"

[[case]]
name = "not granted"
generate = { name = "fetch", args = ["https://auth.example.com/token"] }
expect_error = "plugin fetch: net auth.example.com"
"#;
    plugin_folder(dir.path(), &fixtures::fetcher(), &manifest, cases);
    rustman(dir.path())
        .args(["plugin", "test", "."])
        .assert()
        .code(0)
        .stdout(predicate::str::ends_with("3 passed, 0 failed\n"));
}

/// Canary: a fake secret the plugin returns and logs is redacted in the
/// harness output, while the expectation still compares the real value.
#[test]
fn harness_output_is_redacted() {
    const CANARY: &str = "canary-9f3c1e7d-plugin-secret";
    let dir = tempfile::tempdir().unwrap();
    let manifest = fixtures::manifest("echo", &["generator"], &["secrets:read"]);
    let cases = format!(
        r#"
[fakes]
secrets = {{ api_token = "{CANARY}" }}

[[case]]
name = "echoes"
grants = ["secrets:read:api_token"]
generate = {{ name = "echo" }}
expect = {{ value = "{CANARY}" }}

[[case]]
name = "mismatch"
grants = ["secrets:read:api_token"]
generate = {{ name = "echo" }}
expect = {{ value = "other" }}
"#
    );
    plugin_folder(dir.path(), &fixtures::secret_echo(), &manifest, &cases);
    let output = rustman(dir.path())
        .args(["plugin", "test"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stdout.contains(CANARY) && !stderr.contains(CANARY),
        "{stdout}{stderr}"
    );
    assert!(stdout.contains("ok   echoes\n       value: «redacted:api_token»\n       log info: «redacted:api_token»\n"), "{stdout}");
    assert!(
        stdout.contains("! expected value = \"other\", got \"«redacted:api_token»\""),
        "{stdout}"
    );
}

fn plugins_doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plugins.md");
    std::fs::read_to_string(path).unwrap()
}

/// The fenced block of the doc that starts with `first` (after the fence
/// line), without that fence.
fn doc_block(doc: &str, fence: &str, first: &str) -> String {
    let start = doc
        .find(&format!("```{fence}\n{first}"))
        .unwrap_or_else(|| panic!("no {fence} block starting with {first:?}"))
        + fence.len()
        + 4;
    let end = doc[start..].find("```").unwrap() + start;
    doc[start..end].to_owned()
}

/// The documented test cases, run against an auth component signing like
/// the template's, print the documented output; `plugin api` prints the
/// documented lines.
#[test]
fn the_documented_harness_example_runs_as_shown() {
    let doc = plugins_doc();
    let cases = doc_block(&doc, "toml", "# my-auth/rustman-plugin-tests.toml");
    let output = doc_block(&doc, "text", "plugin my-auth 0.1.0");
    let dir = tempfile::tempdir().unwrap();
    let manifest = fixtures::manifest("my-auth", &["auth"], &["clock", "secrets:read"]);
    plugin_folder(dir.path(), &fixtures::hmac_signer(), &manifest, &cases);
    rustman(dir.path())
        .args(["plugin", "test", "."])
        .assert()
        .code(0)
        .stdout(output);
    let api = doc_block(&doc, "text", "rustman:plugin@");
    rustman(dir.path())
        .args(["plugin", "api"])
        .assert()
        .code(0)
        .stdout(api);
}
