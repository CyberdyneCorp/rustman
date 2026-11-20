//! The `env`, `keychain` and `cmd` providers, and building providers from
//! configuration (secret-providers spec "Keychain and env providers",
//! "Command provider is user-scoped", "Supported provider types").

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use common::{context, fetch, fetch_one};
use rustman_core::secret::Secret;
use rustman_core::secrets::{SecretBackend, SecretStore};
use rustman_secrets::{CmdProvider, EnvProvider, KeychainProvider, build_from};

#[tokio::test]
async fn process_environment_in_ci() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), &[("STRIPE_KEY", "sk_test_canary")]);
    let env = Arc::new(EnvProvider { ctx });
    assert_eq!(
        fetch_one("ci", env.clone(), "ci/STRIPE_KEY").await.unwrap(),
        "sk_test_canary"
    );
    assert_eq!(
        fetch_one("ci", env, "ci/MISSING_KEY").await.unwrap_err(),
        "unresolved secret ci/MISSING_KEY: env: variable 'MISSING_KEY' is not set"
    );
}

#[tokio::test]
async fn keychain_reads_what_secret_set_stored() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join(".gitignore"), ".rustman/\n").unwrap();
    let store = Arc::new(SecretStore::open(ws.path(), SecretBackend::File));
    store
        .set("prod", &Secret::new("api_token", "kc-canary".to_owned()))
        .unwrap();
    let provider = Arc::new(KeychainProvider::with_store(store));
    let value = fetch_one("kc", provider.clone(), "kc/prod/api_token").await;
    assert_eq!(value.unwrap(), "kc-canary");
    let err = fetch_one("kc", provider.clone(), "kc/prod/other")
        .await
        .unwrap_err();
    assert!(
        err.ends_with("(run 'rustman secret set other --env prod')"),
        "{err}"
    );
    let err = fetch_one("kc", provider, "kc/no-slash").await.unwrap_err();
    assert!(
        err.ends_with("keychain: path 'no-slash' must be <env>/<name>"),
        "{err}"
    );
}

/// A script printing `value-for-<arg>`, exiting 3 for `fail`, and the argv
/// that runs it with no shell of ours.
fn script(dir: &Path) -> Vec<String> {
    if cfg!(windows) {
        let path = dir.join("secret.cmd");
        let body = "@echo off\r\nif \"%1\"==\"fail\" exit /b 3\r\necho   value-for-%1  \r\n";
        std::fs::write(&path, body).unwrap();
        vec![path.display().to_string()]
    } else {
        let path = dir.join("secret.sh");
        let body =
            "#!/bin/sh\n[ \"$1\" = fail ] && exit 3\nprintf '  value-for-%s  \\n\\n' \"$1\"\n";
        std::fs::write(&path, body).unwrap();
        vec!["sh".to_owned(), path.display().to_string()]
    }
}

#[tokio::test]
async fn user_defined_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let ctx = context(dir.path(), &[("PATH", &path)]);
    let cmd = Arc::new(CmdProvider {
        name: "pass".into(),
        command: script(dir.path()),
        ctx: ctx.clone(),
    });
    let value = fetch_one("pass", cmd.clone(), "pass/work/api").await;
    assert_eq!(value.unwrap(), "value-for-work/api");
    let err = fetch_one("pass", cmd.clone(), "pass/fail")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "unresolved secret pass/fail: pass: command failed (exit status 3)"
    );
    let err = fetch_one("pass", cmd.clone(), "pass/--output=/tmp/x").await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret pass/--output=/tmp/x: pass: path '--output=/tmp/x' must not start with '-'"
    );
    let missing = Arc::new(CmdProvider {
        name: "pass".into(),
        command: vec!["rustman-test-no-such-command".into()],
        ctx,
    });
    let err = fetch_one("pass", missing, "pass/x").await.unwrap_err();
    assert!(
        err.ends_with("pass: command 'rustman-test-no-such-command' not found"),
        "{err}"
    );
}

fn tables(text: &str) -> BTreeMap<String, toml::Table> {
    let table: toml::Table = toml::from_str(text).unwrap();
    table["secret_providers"]
        .as_table()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_table().unwrap().clone()))
        .collect()
}

#[tokio::test]
async fn every_built_in_type_builds_from_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = tables(
        r#"
[secret_providers.ci]
type = "env"
[secret_providers.kc]
type = "keychain"
[secret_providers.sops]
type = "sops"
[secret_providers.op]
type = "onepassword"
[secret_providers.vault]
type = "vault"
address = "http://127.0.0.1:8200"
auth = "approle"
"#,
    );
    let user = tables(
        r#"
[secret_providers.pass]
type = "cmd"
command = ["pass", "show"]
[secret_providers.op]
op_path = "/opt/1password/op"
"#,
    );
    let ctx = context(dir.path(), &[("RUSTMAN_SECRET_STORE", "file")]);
    let providers = build_from(&workspace, &user, &ctx).unwrap();
    assert_eq!(
        providers.names(),
        ["ci", "kc", "op", "pass", "sops", "vault"]
    );
    let vault = providers
        .infos()
        .into_iter()
        .find(|i| i.name == "vault")
        .unwrap();
    assert_eq!(vault.auth.as_deref(), Some("approle"));
    let pass = providers
        .infos()
        .into_iter()
        .find(|i| i.name == "pass")
        .unwrap();
    assert_eq!(pass.origin.to_string(), "user");

    let bad = tables("[secret_providers.pass]\ntype = \"cmd\"\ncommand = []\n");
    let err = build_from(&BTreeMap::new(), &bad, &ctx).err().unwrap();
    assert!(
        err.to_string()
            .contains("'command' must be a non-empty list"),
        "{err}"
    );
    // A Vault without an address still builds: only its fetches fail.
    let lazy = tables("[secret_providers.v]\ntype = \"vault\"\n");
    let providers = build_from(&lazy, &BTreeMap::new(), &ctx).unwrap();
    let err = fetch(&providers, "v/kv/app").await.unwrap_err();
    assert!(
        err.to_string()
            .ends_with("vault: no address (set 'address' or VAULT_ADDR)"),
        "{err}"
    );
}

/// "Cloud type in a minimal build": the cloud features are not built
/// here, so every cloud type names the feature it needs.
#[tokio::test]
async fn cloud_type_in_a_minimal_build() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = tables(
        r#"
[secret_providers.aws]
type = "aws-secrets-manager"
region = "eu-west-1"
[secret_providers.ssm]
type = "aws-ssm"
[secret_providers.gcp]
type = "gcp-secret-manager"
project = "p"
[secret_providers.az]
type = "azure-key-vault"
vault = "v"
"#,
    );
    let ctx = context(dir.path(), &[]);
    let providers = build_from(&workspace, &BTreeMap::new(), &ctx).unwrap();
    let cases = [
        ("aws/prod/db#password", "aws-secrets-manager", "secrets-aws"),
        ("ssm/app/key", "aws-ssm", "secrets-aws"),
        ("gcp/key", "gcp-secret-manager", "secrets-gcp"),
        ("az/key", "azure-key-vault", "secrets-azure"),
    ];
    let compiled_out = cases.iter().filter(|c| !rustman_secrets::is_built(c.2));
    for (reference, kind, feature) in compiled_out {
        let err = fetch(&providers, reference).await.unwrap_err();
        assert_eq!(
            err.error.to_string(),
            format!(
                "secret provider type '{kind}' is not available in this build (feature {feature})"
            )
        );
        assert_eq!(err.exit_code(), 2);
    }
}

/// A timed-out command is killed with the programs it started: a script
/// that waits on `sleep 30` leaves no `sleep` behind.
#[cfg(unix)]
#[tokio::test]
async fn a_timed_out_command_takes_its_children_with_it() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let path = dir.path().join("slow.sh");
    let body = format!(
        "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
        pid_file.display()
    );
    std::fs::write(&path, body).unwrap();
    let env_path = std::env::var("PATH").unwrap_or_default();
    let cmd = Arc::new(CmdProvider {
        name: "slow".into(),
        command: vec!["sh".to_owned(), path.display().to_string()],
        ctx: context(dir.path(), &[("PATH", &env_path)]),
    });
    let providers = common::registry("slow", cmd, Duration::from_millis(1500));
    let err = fetch(&providers, "slow/x").await.unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !alive(),
        "the child of the timed-out command is still running"
    );
}
