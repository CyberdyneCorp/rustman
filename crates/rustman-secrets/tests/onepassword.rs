//! The `onepassword` provider (secret-providers spec "1Password provider"):
//! a fake `op` on `PATH` for the CLI, a stub Connect server for Connect.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{context, fetch_one};
use rustman_secrets::OnePassword;
use rustman_test_support::{GraphqlHttpServer, Reply};
use serde_json::json;

const TOKEN: &str = "op-canary-token-5e2a";

/// A fake `op` in `dir` that prints `TOKEN` for
/// `read op://Engineering/Stripe/token`, reports a missing item for other
/// paths, or (`signed_in` false) reports a signed-out CLI. Returns the
/// program to run: `op` on `PATH` on Unix; the `.cmd` shim itself on
/// Windows, where a program name is only searched as `.exe`.
fn fake_op(dir: &Path, signed_in: bool) -> String {
    if cfg!(windows) {
        let body = if signed_in {
            format!(
                "@echo off\r\nif \"%1\"==\"whoami\" (echo ok& exit /b 0)\r\nif \"%2\"==\"op://Engineering/Stripe/token\" (echo {TOKEN}& exit /b 0)\r\necho [ERROR] \"Nope\" isn't an item in the \"Engineering\" vault 1>&2\r\nexit /b 1\r\n"
            )
        } else {
            "@echo off\r\necho [ERROR] You are not currently signed in. Please run `op signin --help` 1>&2\r\nexit /b 1\r\n".to_owned()
        };
        let path = dir.join("op.cmd");
        std::fs::write(&path, body).unwrap();
        return path.display().to_string();
    }
    let body = if signed_in {
        format!(
            "#!/bin/sh\nif [ \"$1\" = whoami ]; then echo ok; exit 0; fi\nif [ \"$1 $2\" = \"read op://Engineering/Stripe/token\" ]; then echo '{TOKEN}'; exit 0; fi\necho \"[ERROR] \\\"Nope\\\" isn't an item in the \\\"Engineering\\\" vault\" >&2\nexit 1\n"
        )
    } else {
        "#!/bin/sh\necho '[ERROR] You are not currently signed in. Please run `op signin --help`' >&2\nexit 1\n".to_owned()
    };
    let path = dir.join("op");
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    "op".to_owned()
}

fn cli(dir: &Path, signed_in: bool) -> Arc<OnePassword> {
    let op = fake_op(dir, signed_in);
    let path = dir.display().to_string();
    let ctx = context(dir, &[("PATH", &path)]);
    Arc::new(OnePassword::cli(ctx, op, None))
}

#[tokio::test]
async fn op_cli() {
    let dir = tempfile::tempdir().unwrap();
    let value = fetch_one("op", cli(dir.path(), true), "op/Engineering/Stripe/token").await;
    assert_eq!(value.unwrap(), TOKEN);
    let err = fetch_one("op", cli(dir.path(), true), "op/Engineering/Nope/token").await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret op/Engineering/Nope/token: onepassword: Engineering/Nope/token not found"
    );
}

/// The CLI checks the path like Connect does, before running `op`.
#[tokio::test]
async fn op_cli_paths_need_vault_item_and_field() {
    let dir = tempfile::tempdir().unwrap();
    for reference in ["op/Engineering/Stripe", "op/Engineering//token"] {
        // Signed out: running `op` would answer "not signed in".
        let err = fetch_one("op", cli(dir.path(), false), reference).await;
        let path = reference.trim_start_matches("op/");
        assert_eq!(
            err.unwrap_err(),
            format!(
                "unresolved secret {reference}: onepassword: path '{path}' must be <vault>/<item>/<field>"
            )
        );
    }
}

#[tokio::test]
async fn op_not_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let err = fetch_one("op", cli(dir.path(), false), "op/Engineering/Stripe/token").await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret op/Engineering/Stripe/token: onepassword: not signed in (run 'op signin')"
    );
}

/// `secret test` runs `op whoami`.
#[tokio::test]
async fn op_health_check() {
    let dir = tempfile::tempdir().unwrap();
    let check = |provider: Arc<OnePassword>| async move {
        common::registry("op", provider, Duration::from_secs(20))
            .check("op")
            .await
    };
    assert_eq!(check(cli(dir.path(), true)).await, Ok(()));
    let err = check(cli(dir.path(), false)).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "onepassword: not signed in (run 'op signin')"
    );
}

#[tokio::test]
async fn a_missing_op_names_the_user_config_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().display().to_string();
    let ctx = context(dir.path(), &[("PATH", &path)]);
    let provider = Arc::new(OnePassword::cli(
        ctx,
        "rustman-test-no-such-op".into(),
        None,
    ));
    let err = fetch_one("op", provider, "op/a/b/c").await.unwrap_err();
    assert!(err.contains("set op_path in user config"), "{err}");
}

fn connect_server() -> impl Fn(&rustman_test_support::RecordedRequest) -> Reply + Send + Sync {
    |req| {
        if req.header("authorization") != Some("Bearer connect-token") {
            return Reply::raw(
                401,
                "application/json",
                r#"{"status":401,"message":"Invalid token"}"#,
            );
        }
        let path = req.uri.split('?').next().unwrap_or_default();
        let filter = req
            .query_pairs()
            .into_iter()
            .find(|(k, _)| k == "filter")
            .map(|(_, v)| v);
        let body = match (path, filter.as_deref()) {
            ("/v1/vaults", Some(r#"name eq "Engineering""#)) => {
                json!([{"id": "v1", "name": "Engineering"}])
            }
            ("/v1/vaults/v1/items", Some(r#"title eq "Stripe""#)) => {
                json!([{"id": "i1", "title": "Stripe"}])
            }
            ("/v1/vaults/v1/items/i1", None) => json!({"id": "i1", "fields": [
                {"id": "username", "label": "username", "value": "ops"},
                {"id": "f2", "label": "token", "value": TOKEN}
            ]}),
            _ => json!([]),
        };
        Reply::raw(200, "application/json", &body.to_string())
    }
}

#[tokio::test]
async fn connect_server_reads_fields_with_the_env_token() {
    let server = GraphqlHttpServer::start(connect_server()).await;
    let dir = tempfile::tempdir().unwrap();
    let host = server.url("");
    let connect = |token: Option<&str>| {
        let vars: Vec<(&str, &str)> = token.map(|t| ("OP_CONNECT_TOKEN", t)).into_iter().collect();
        let ctx = context(dir.path(), &vars);
        Arc::new(OnePassword::connect(ctx, &host, Duration::from_secs(5)))
    };
    let value = fetch_one(
        "op",
        connect(Some("connect-token")),
        "op/Engineering/Stripe/token",
    )
    .await;
    assert_eq!(value.unwrap(), TOKEN);
    let err = fetch_one(
        "op",
        connect(Some("connect-token")),
        "op/Engineering/Stripe/nope",
    )
    .await;
    assert_eq!(
        err.unwrap_err(),
        "unresolved secret op/Engineering/Stripe/nope: field 'nope' not found"
    );
    let err = fetch_one(
        "op",
        connect(Some("wrong-token")),
        "op/Engineering/Stripe/token",
    )
    .await;
    let err = err.unwrap_err();
    assert!(err.ends_with("onepassword: authentication failed"), "{err}");
    assert!(!err.contains("wrong-token"));
    let check = |provider: Arc<OnePassword>| async move {
        common::registry("op", provider, Duration::from_secs(5))
            .check("op")
            .await
    };
    assert_eq!(check(connect(Some("connect-token"))).await, Ok(()));
    let err = check(connect(Some("wrong-token"))).await.unwrap_err();
    assert_eq!(err.to_string(), "onepassword: authentication failed");
    let err = fetch_one("op", connect(None), "op/Engineering/Stripe/token").await;
    assert!(
        err.unwrap_err()
            .ends_with("onepassword: OP_CONNECT_TOKEN is not set")
    );
    let err = fetch_one(
        "op",
        connect(Some("connect-token")),
        "op/Other/Stripe/token",
    )
    .await;
    assert!(
        err.unwrap_err()
            .ends_with("onepassword: Other/Stripe/token not found")
    );
}

/// A cloned workspace picks the Connect host: `OP_CONNECT_TOKEN` is never
/// sent there unless the user chose it (`OP_CONNECT_HOST` or user config).
#[tokio::test]
async fn a_workspace_connect_host_never_receives_the_token() {
    let server = GraphqlHttpServer::start(connect_server()).await;
    let dir = tempfile::tempdir().unwrap();
    let host = server.url("");
    let host = host.trim_end_matches('/');
    let workspace =
        format!("[secret_providers.op]\ntype = \"onepassword\"\nconnect_host = \"{host}\"\n");
    let ctx = context(dir.path(), &[("OP_CONNECT_TOKEN", "connect-token")]);
    let providers = common::configured(&workspace, "", &ctx);
    let err = common::fetch(&providers, "op/Engineering/Stripe/token")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string().ends_with(&format!(
            "onepassword: connect_host {host} is set in rustman.toml; set OP_CONNECT_HOST to it, or 'connect_host' in user config, to send OP_CONNECT_TOKEN there"
        )),
        "{err}"
    );
    assert!(providers.check("op").await.is_err());
    assert!(server.hits().is_empty(), "the workspace host was contacted");

    let ctx = context(
        dir.path(),
        &[
            ("OP_CONNECT_TOKEN", "connect-token"),
            ("OP_CONNECT_HOST", host),
        ],
    );
    let providers = common::configured(&workspace, "", &ctx);
    let value = common::fetch(&providers, "op/Engineering/Stripe/token").await;
    assert_eq!(value.unwrap(), TOKEN);
    let ctx = context(dir.path(), &[("OP_CONNECT_TOKEN", "connect-token")]);
    let user = format!("[secret_providers.op]\nconnect_host = \"{host}\"\n");
    let providers = common::configured(&workspace, &user, &ctx);
    let value = common::fetch(&providers, "op/Engineering/Stripe/token").await;
    assert_eq!(value.unwrap(), TOKEN);
}
