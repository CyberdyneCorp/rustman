//! The `sops` provider (secret-providers spec "SOPS provider"): age files
//! decrypted natively, PGP through `gpg`, the MAC checked, and paths kept
//! inside the workspace.
//!
//! Fixtures are written in the SOPS format with this crate's encoder and
//! the `age` crate; `real_sops_files_decrypt` checks a file the `sops`
//! binary encrypted whenever `sops` and `age-keygen` are installed (CI).

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use age::secrecy::ExposeSecret;
use age::x25519;
use common::{context, fetch_one};
use rustman_secrets::sops::tree::encrypt;
use rustman_secrets::{Context, Sops};
use serde_json::{Value, json};

const DATA_KEY: [u8; 32] = [42; 32];
const MODIFIED: &str = "2026-03-04T05:06:07Z";
const CANARY: &str = "sops-canary-7c1f";

/// The document of `secrets/prod.sops.yaml` before encryption.
fn plain() -> Value {
    json!({"db": {"password": CANARY, "port": 5432}, "api": {"keys": ["k1", "k2"]}})
}

/// A SOPS YAML file of `plain()` whose data key is wrapped by `stanzas`.
fn sops_yaml(stanzas: Value) -> String {
    let (encrypted, mac) = encrypt::document(&plain(), &DATA_KEY, MODIFIED);
    let mut doc = encrypted.as_object().unwrap().clone();
    let mut sops = json!({"lastmodified": MODIFIED, "mac": mac, "version": "3.9.0"});
    for (k, v) in stanzas.as_object().unwrap() {
        sops[k] = v.clone();
    }
    doc.insert("sops".into(), sops);
    // YAML is a superset of JSON: SOPS' YAML reader takes it as is.
    serde_json::to_string_pretty(&Value::Object(doc)).unwrap()
}

/// An age identity, its key file in `dir`, and a stanza for it.
fn age_identity(dir: &Path) -> (std::path::PathBuf, Value) {
    let identity = x25519::Identity::generate();
    let keys = dir.join("keys.txt");
    std::fs::write(&keys, format!("{}\n", identity.to_string().expose_secret())).unwrap();
    let recipient = identity.to_public();
    let enc = age::encrypt_and_armor(&recipient, &DATA_KEY).unwrap();
    (
        keys,
        json!({"age": [{"recipient": recipient.to_string(), "enc": enc}]}),
    )
}

fn workspace_with(file: &str, text: &str) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let path = ws.path().join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    ws
}

fn provider(ctx: Context, gpg: Option<&str>) -> Arc<Sops> {
    Arc::new(Sops::new(ctx, gpg))
}

#[tokio::test]
async fn age_encrypted_file() {
    let keys_dir = tempfile::tempdir().unwrap();
    let (keys, stanza) = age_identity(keys_dir.path());
    let ws = workspace_with("secrets/prod.sops.yaml", &sops_yaml(stanza));
    let keys = keys.display().to_string();
    let ctx = context(ws.path(), &[("SOPS_AGE_KEY_FILE", &keys)]);
    let reference = "sops/secrets/prod.sops.yaml#db.password";
    let value = fetch_one("sops", provider(ctx.clone(), None), reference).await;
    assert_eq!(value.unwrap(), CANARY);
    let port = fetch_one(
        "sops",
        provider(ctx.clone(), None),
        "sops/secrets/prod.sops.yaml#db.port",
    );
    assert_eq!(port.await.unwrap(), "5432");
    let key = fetch_one(
        "sops",
        provider(ctx, None),
        "sops/secrets/prod.sops.yaml#api.keys.1",
    );
    assert_eq!(key.await.unwrap(), "k2");
}

#[tokio::test]
async fn inline_age_keys_and_json_files_work() {
    let keys_dir = tempfile::tempdir().unwrap();
    let (keys, stanza) = age_identity(keys_dir.path());
    let ws = workspace_with("prod.sops.json", &sops_yaml(stanza));
    let inline = std::fs::read_to_string(keys).unwrap();
    let ctx = context(ws.path(), &[("SOPS_AGE_KEY", &inline)]);
    let value = fetch_one("s", provider(ctx, None), "s/prod.sops.json#db.password").await;
    assert_eq!(value.unwrap(), CANARY);
}

#[tokio::test]
async fn path_escape() {
    let ws = tempfile::tempdir().unwrap();
    let ctx = context(ws.path(), &[]);
    let err = fetch_one("sops", provider(ctx, None), "sops/../other/keys.yaml#k").await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret sops/../other/keys.yaml#k: sops: path escapes the workspace"
    );
}

#[tokio::test]
async fn a_tampered_file_fails_its_mac() {
    let keys_dir = tempfile::tempdir().unwrap();
    let (keys, stanza) = age_identity(keys_dir.path());
    let text = sops_yaml(stanza);
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    doc["api"]["keys"].as_array_mut().unwrap().pop();
    let ws = workspace_with("s.yaml", &doc.to_string());
    let keys = keys.display().to_string();
    let ctx = context(ws.path(), &[("SOPS_AGE_KEY_FILE", &keys)]);
    let err = fetch_one("sops", provider(ctx, None), "sops/s.yaml#db.password").await;
    let err = err.unwrap_err();
    assert!(err.contains("sops: MAC mismatch in s.yaml"), "{err}");
    assert!(!err.contains(CANARY));
}

#[tokio::test]
async fn without_an_identity_the_error_names_the_key_file_variable() {
    let keys_dir = tempfile::tempdir().unwrap();
    let (_, stanza) = age_identity(keys_dir.path());
    let ws = workspace_with("s.yaml", &sops_yaml(stanza));
    let empty = tempfile::tempdir().unwrap();
    let home = empty.path().display().to_string();
    let ctx = context(
        ws.path(),
        &[
            ("HOME", &home),
            ("XDG_CONFIG_HOME", &home),
            ("APPDATA", &home),
        ],
    );
    let err = fetch_one("sops", provider(ctx, None), "sops/s.yaml#db.password").await;
    assert!(err.unwrap_err().contains("set SOPS_AGE_KEY_FILE"));
}

#[tokio::test]
async fn pgp_without_gpg() {
    let stanza = json!({"pgp": [{"fp": "ABCD", "enc": "-----BEGIN PGP MESSAGE-----\nx\n-----END PGP MESSAGE-----\n"}]});
    let ws = workspace_with("pgp.sops.yaml", &sops_yaml(stanza));
    let ctx = context(ws.path(), &[]);
    let gpg = Some("rustman-test-no-such-gpg");
    let err = fetch_one("sops", provider(ctx, gpg), "sops/pgp.sops.yaml#db.password").await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret sops/pgp.sops.yaml#db.password: sops: PGP decryption requires gpg, which was not found"
    );
}

fn installed(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn pgp_files_decrypt_through_gpg() {
    if !installed("gpg") {
        eprintln!("skipped: gpg is not installed");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let gnupghome = home.path().display().to_string();
    let gpg = |args: &[&str], input: Option<&[u8]>| {
        use std::io::Write;
        let mut child = Command::new("gpg")
            .env("GNUPGHOME", &gnupghome)
            .args([
                "--batch",
                "--quiet",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
            ])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        drop(child.stdin.take());
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "gpg {args:?} failed");
        out.stdout
    };
    let uid = "rustman-test@example.invalid";
    gpg(
        &["--quick-gen-key", uid, "default", "default", "never"],
        None,
    );
    let enc = gpg(
        &["--trust-model", "always", "--armor", "--encrypt", "-r", uid],
        Some(&DATA_KEY),
    );
    let enc = String::from_utf8(enc).unwrap();
    let stanza = json!({"pgp": [{"fp": "TEST", "enc": enc}]});
    let ws = workspace_with("pgp.sops.yaml", &sops_yaml(stanza));
    let path = std::env::var("PATH").unwrap_or_default();
    let ctx = context(ws.path(), &[("GNUPGHOME", &gnupghome), ("PATH", &path)]);
    let value = fetch_one(
        "sops",
        provider(ctx, None),
        "sops/pgp.sops.yaml#db.password",
    )
    .await;
    assert_eq!(value.unwrap(), CANARY);
}

/// A file encrypted by the real `sops` binary, when it is installed.
#[tokio::test]
async fn real_sops_files_decrypt() {
    let tools = installed("sops") && Command::new("age-keygen").arg("--version").output().is_ok();
    if !tools {
        // CI installs both and sets RUSTMAN_REQUIRE_SOPS, so it cannot skip.
        assert!(
            std::env::var_os("RUSTMAN_REQUIRE_SOPS").is_none(),
            "RUSTMAN_REQUIRE_SOPS is set but sops or age-keygen is missing"
        );
        eprintln!("skipped: sops and age-keygen are not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys.txt");
    let status = Command::new("age-keygen")
        .arg("-o")
        .arg(&keys)
        .output()
        .unwrap();
    assert!(status.status.success());
    let keys_text = std::fs::read_to_string(&keys).unwrap();
    let recipient = keys_text
        .lines()
        .find_map(|l| l.strip_prefix("# public key: "))
        .unwrap()
        .to_owned();
    let plain = dir.path().join("prod.yaml");
    std::fs::write(
        &plain,
        format!("db:\n  password: {CANARY}\n  port: 5432\n  tls: true\nlist:\n  - a\n  - 1.5\n"),
    )
    .unwrap();
    let out = Command::new("sops")
        .args(["--encrypt", "--age", &recipient])
        .arg(&plain)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ws = workspace_with(
        "secrets/prod.sops.yaml",
        &String::from_utf8(out.stdout).unwrap(),
    );
    let keys = keys.display().to_string();
    let ctx = context(ws.path(), &[("SOPS_AGE_KEY_FILE", &keys)]);
    for (field, expected) in [
        ("db.password", CANARY),
        ("db.tls", "true"),
        ("list.1", "1.5"),
    ] {
        let reference = format!("sops/secrets/prod.sops.yaml#{field}");
        let value = fetch_one("sops", provider(ctx.clone(), None), &reference).await;
        assert_eq!(value.unwrap(), expected, "{field}");
    }
}
