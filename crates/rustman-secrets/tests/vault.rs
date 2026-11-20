//! The `vault` provider (secret-providers spec "Vault provider") against a
//! stub of Vault's HTTP API: KV v1/v2 detection, versions, namespaces and
//! token, AppRole and OIDC auth.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use common::{context, fetch_one};
use rustman_secrets::vault::AuthMethod;
use rustman_secrets::{Context, Vault};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const TOKEN: &str = "hvs.canary-token-81d2";

fn ok(body: Value) -> Reply {
    Reply::raw(200, "application/json", &body.to_string())
}

fn mount(path: &str, version: &str) -> Reply {
    ok(json!({"data": {"path": path, "type": "kv", "options": {"version": version}}}))
}

/// Vault with a KV v2 mount `kv` and a KV v1 mount `secret`, accepting
/// `TOKEN` or the AppRole/OIDC login tokens.
fn vault_stub(req: &RecordedRequest) -> Reply {
    let path = req.uri.split('?').next().unwrap_or_default();
    match path {
        "/v1/auth/approle/login" => {
            let body = req.json();
            if body["role_id"] == "role" && body["secret_id"] == "sid" {
                return ok(json!({"auth": {"client_token": "s.approle"}}));
            }
            return Reply::raw(
                400,
                "application/json",
                r#"{"errors":["invalid secret id"]}"#,
            );
        }
        "/v1/auth/oidc/oidc/auth_url" => {
            let redirect = req.json()["redirect_uri"].as_str().unwrap().to_owned();
            return ok(json!({"data": {"auth_url": format!("{redirect}?state=st1&code=c1")}}));
        }
        "/v1/auth/oidc/oidc/callback" => {
            let q = req.query_pairs();
            let has = |k: &str, v: &str| q.iter().any(|(a, b)| a == k && b == v);
            if has("state", "st1")
                && has("code", "c1")
                && q.iter().any(|(a, _)| a == "client_nonce")
            {
                return ok(json!({"auth": {"client_token": "s.oidc"}}));
            }
            return Reply::raw(400, "application/json", "{}");
        }
        _ => {}
    }
    let token = req.header("x-vault-token").unwrap_or_default();
    if ![TOKEN, "s.approle", "s.oidc"].contains(&token) {
        return Reply::raw(
            403,
            "application/json",
            r#"{"errors":["permission denied"]}"#,
        );
    }
    match path {
        "/v1/sys/internal/ui/mounts/kv/app/db" | "/v1/sys/internal/ui/mounts/kv/app/locked" => {
            mount("kv/", "2")
        }
        "/v1/sys/internal/ui/mounts/secret/legacy" => mount("secret/", "1"),
        "/v1/kv/data/app/db" => {
            let version = req
                .query_pairs()
                .into_iter()
                .find(|(k, _)| k == "version")
                .map(|(_, v)| v);
            let password = match version.as_deref() {
                Some("3") => "pw-v3",
                _ => "pw-latest",
            };
            let ns = req.header("x-vault-namespace").unwrap_or("none").to_owned();
            ok(
                json!({"data": {"data": {"password": password, "ns": ns}, "metadata": {"version": 4}}}),
            )
        }
        "/v1/kv/data/app/locked" => Reply::raw(
            403,
            "application/json",
            r#"{"errors":["1 error occurred:\n\t* permission denied\n\n"]}"#,
        ),
        "/v1/secret/legacy" => ok(json!({"data": {"user": "legacy-user"}})),
        "/v1/auth/token/lookup-self" => ok(json!({"data": {"ttl": 3600}})),
        _ => Reply::raw(404, "application/json", r#"{"errors":[]}"#),
    }
}

fn vault(address: &str, auth: AuthMethod, ctx: Context, namespace: Option<&str>) -> Arc<Vault> {
    let timeout = Duration::from_secs(10);
    Arc::new(Vault::new(
        "vault",
        Some(address),
        namespace,
        auth,
        ctx,
        timeout,
    ))
}

#[tokio::test]
async fn kv_v2_with_version() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), &[("VAULT_TOKEN", TOKEN)]);
    let address = server.url("");
    let address = address.trim_end_matches('/');
    let v = vault(address, AuthMethod::Token, ctx.clone(), None);
    let value = fetch_one("vault", v.clone(), "vault/kv/app/db?version=3#password").await;
    assert_eq!(value.unwrap(), "pw-v3");
    let value = fetch_one("vault", v, "vault/kv/app/db#password").await;
    assert_eq!(value.unwrap(), "pw-latest");
    let namespaced = vault(address, AuthMethod::Token, ctx.clone(), Some("team-a"));
    let value = fetch_one("vault", namespaced, "vault/kv/app/db#ns").await;
    assert_eq!(value.unwrap(), "team-a");
    let v1 = vault(address, AuthMethod::Token, ctx.clone(), None);
    let value = fetch_one("vault", v1.clone(), "vault/secret/legacy#user").await;
    assert_eq!(value.unwrap(), "legacy-user");
    let err = fetch_one("vault", v1, "vault/secret/legacy?version=2#user").await;
    assert!(err.unwrap_err().contains("?version needs a KV v2 mount"));
    let hits = server.hits();
    assert!(
        hits.iter()
            .all(|h| h.header("x-vault-token") == Some(TOKEN))
    );
}

#[tokio::test]
async fn permission_denied_never_shows_the_token() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), &[("VAULT_TOKEN", TOKEN)]);
    let address = server.url("");
    let v = vault(address.trim_end_matches('/'), AuthMethod::Token, ctx, None);
    let err = fetch_one("vault", v, "vault/kv/app/locked#password")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "unresolved secret vault/kv/app/locked#password: vault: permission denied for kv/app/locked"
    );
    assert!(!err.contains(TOKEN));
    let missing = fetch_one(
        "vault",
        vault(
            address.trim_end_matches('/'),
            AuthMethod::Token,
            context(dir.path(), &[("VAULT_TOKEN", TOKEN)]),
            None,
        ),
        "vault/kv/app/db#user",
    )
    .await;
    assert_eq!(
        missing.unwrap_err(),
        "unresolved secret vault/kv/app/db#user: field 'user' not found"
    );
}

#[tokio::test]
async fn tokens_come_from_the_environment_or_the_token_file() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let home = tempfile::tempdir().unwrap();
    let address = server.url("");
    let address = address.trim_end_matches('/');
    let home_path = home.path().display().to_string();
    let vars = [
        ("HOME", home_path.as_str()),
        ("USERPROFILE", home_path.as_str()),
    ];
    let err = fetch_one(
        "vault",
        vault(
            address,
            AuthMethod::Token,
            context(home.path(), &vars),
            None,
        ),
        "vault/kv/app/db#password",
    )
    .await;
    assert!(
        err.unwrap_err()
            .ends_with("vault: no token (set VAULT_TOKEN or run 'vault login')")
    );
    std::fs::write(home.path().join(".vault-token"), format!("{TOKEN}\n")).unwrap();
    let value = fetch_one(
        "vault",
        vault(
            address,
            AuthMethod::Token,
            context(home.path(), &vars),
            None,
        ),
        "vault/kv/app/db#password",
    )
    .await;
    assert_eq!(value.unwrap(), "pw-latest");
}

#[tokio::test]
async fn approle_logs_in_once_with_ids_from_the_environment() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let address = server.url("");
    let address = address.trim_end_matches('/');
    let auth = AuthMethod::AppRole {
        mount: "approle".into(),
    };
    let ctx = context(
        dir.path(),
        &[("VAULT_ROLE_ID", "role"), ("VAULT_SECRET_ID", "sid")],
    );
    let v = vault(address, auth.clone(), ctx, None);
    for reference in ["vault/kv/app/db#password", "vault/secret/legacy#user"] {
        assert!(fetch_one("vault", v.clone(), reference).await.is_ok());
    }
    let logins = server
        .hits()
        .iter()
        .filter(|h| h.uri.contains("/login"))
        .count();
    assert_eq!(logins, 1);
    let bad = context(
        dir.path(),
        &[("VAULT_ROLE_ID", "role"), ("VAULT_SECRET_ID", "wrong-sid")],
    );
    let err = fetch_one(
        "vault",
        vault(address, auth, bad, None),
        "vault/kv/app/db#password",
    )
    .await;
    let err = err.unwrap_err();
    assert!(err.ends_with("vault: approle login failed"), "{err}");
    assert!(!err.contains("wrong-sid"));
}

/// A browser that follows the login URL: a GET to the loopback callback.
fn follow(url: &str) -> std::io::Result<()> {
    let rest = url
        .strip_prefix("http://localhost:")
        .expect("a loopback URL");
    let (port, target) = rest.split_once('/').unwrap();
    let target = format!("/{target}");
    let port: u16 = port.parse().unwrap();
    std::thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
    });
    Ok(())
}

#[tokio::test]
async fn oidc_logs_in_through_the_browser_once() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let address = server.url("");
    let auth = AuthMethod::Oidc {
        mount: "oidc".into(),
        role: Some("dev".into()),
        port: 0,
    };
    let ctx = context(dir.path(), &[]).with_browser(follow);
    let v = vault(address.trim_end_matches('/'), auth, ctx, None);
    for _ in 0..2 {
        let value = fetch_one("vault", v.clone(), "vault/kv/app/db#password").await;
        assert_eq!(value.unwrap(), "pw-latest");
    }
    let hits = server.hits();
    let auth_url = hits.iter().find(|h| h.uri.ends_with("/auth_url")).unwrap();
    assert_eq!(auth_url.json()["role"], "dev");
    assert_eq!(
        hits.iter()
            .filter(|h| h.uri.contains("/oidc/callback"))
            .count(),
        1
    );
    assert!(
        hits.iter()
            .filter(|h| h.uri.starts_with("/v1/kv/"))
            .all(|h| h.header("x-vault-token") == Some("s.oidc"))
    );
}

/// "Provider health check": `secret test` looks the token up; an expired
/// token is an auth failure that never shows the token.
#[tokio::test]
async fn provider_health_check() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let address = server.url("");
    let address = address.trim_end_matches('/');
    let check = |token: &str| {
        let ctx = context(dir.path(), &[("VAULT_TOKEN", token)]);
        let v = vault(address, AuthMethod::Token, ctx, None);
        async move {
            common::registry("vault", v, Duration::from_secs(10))
                .check("vault")
                .await
        }
    };
    assert_eq!(check(TOKEN).await, Ok(()));
    let err = check("hvs.expired-token").await.unwrap_err();
    assert_eq!(err.to_string(), "vault: authentication failed");
    assert_eq!(err.exit_code(), 3);
}

/// A cloned workspace picks the Vault address: the user's own token
/// (`VAULT_TOKEN`, `~/.vault-token`) is never sent there unless the user
/// chose that address too (`VAULT_ADDR` or the user config).
#[tokio::test]
async fn a_workspace_address_never_receives_the_users_token() {
    let server = GraphqlHttpServer::start(vault_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let address = server.url("");
    let address = address.trim_end_matches('/');
    let workspace =
        format!("[secret_providers.vault]\ntype = \"vault\"\naddress = \"{address}\"\n");
    let ctx = context(dir.path(), &[("VAULT_TOKEN", TOKEN)]);
    let providers = common::configured(&workspace, "", &ctx);
    let err = common::fetch(&providers, "vault/kv/app/db#password")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string().ends_with(&format!(
            "vault: address {address} is set in rustman.toml; set VAULT_ADDR to it, or 'address' in user config, to send your Vault credentials there"
        )),
        "{err}"
    );
    assert!(!err.to_string().contains(TOKEN));
    let err = providers.check("vault").await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(server.hits().is_empty(), "the workspace host was contacted");

    let approle = format!("{workspace}auth = \"approle\"\n");
    let ctx = context(
        dir.path(),
        &[("VAULT_ROLE_ID", "role"), ("VAULT_SECRET_ID", "sid")],
    );
    let providers = common::configured(&approle, "", &ctx);
    assert!(
        common::fetch(&providers, "vault/kv/app/db#password")
            .await
            .is_err()
    );
    assert!(server.hits().is_empty(), "the AppRole ids were sent");

    // The user chose that address: VAULT_ADDR matches, or the user config sets it.
    let slash = format!("{address}/");
    let ctx = context(
        dir.path(),
        &[("VAULT_TOKEN", TOKEN), ("VAULT_ADDR", slash.as_str())],
    );
    let providers = common::configured(&workspace, "", &ctx);
    let value = common::fetch(&providers, "vault/kv/app/db#password").await;
    assert_eq!(value.unwrap(), "pw-latest");
    let ctx = context(dir.path(), &[("VAULT_TOKEN", TOKEN)]);
    let user = format!("[secret_providers.vault]\naddress = \"{address}\"\n");
    let ws =
        "[secret_providers.vault]\ntype = \"vault\"\naddress = \"https://elsewhere.invalid\"\n";
    let providers = common::configured(ws, &user, &ctx);
    let value = common::fetch(&providers, "vault/kv/app/db#password").await;
    assert_eq!(value.unwrap(), "pw-latest");
}

/// Task 3.4 against a real `vault server -dev`: the CI job writes two
/// versions of `secret/app` (KV v2) and creates a token without access to
/// it. Skipped unless `RUSTMAN_VAULT_ADDR` is set.
#[tokio::test]
async fn vault_dev_server() {
    let Ok(address) = std::env::var("RUSTMAN_VAULT_ADDR") else {
        eprintln!("skipped: RUSTMAN_VAULT_ADDR is not set (the vault-dev CI job sets it)");
        return;
    };
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let (root, limited) = (
        var("RUSTMAN_VAULT_TOKEN"),
        var("RUSTMAN_VAULT_LIMITED_TOKEN"),
    );
    let dir = tempfile::tempdir().unwrap();
    let with_token = |token: &str| {
        let ctx = context(dir.path(), &[("VAULT_TOKEN", token)]);
        vault(&address, AuthMethod::Token, ctx, None)
    };
    // "KV v2 with version"
    let v = with_token(&root);
    let value = fetch_one("vault", v.clone(), "vault/secret/app?version=1#password").await;
    assert_eq!(value.unwrap(), "dev-password-v1");
    let value = fetch_one("vault", v, "vault/secret/app#password").await;
    assert_eq!(value.unwrap(), "dev-password-v2");
    // "Permission denied", with no token in the message
    let err = fetch_one("vault", with_token(&limited), "vault/secret/app#password")
        .await
        .unwrap_err();
    assert!(
        err.ends_with("vault: permission denied for secret/app"),
        "{err}"
    );
    assert!(!err.contains(&limited) && !err.contains(&root));
}

/// A shared `rustman.toml` naming Vault without `address` still loads for
/// someone without `VAULT_ADDR`: only a reference to it fails, when sent.
#[tokio::test]
async fn a_missing_address_fails_only_the_references_to_vault() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        "[secret_providers.vault]\ntype = \"vault\"\n\n[secret_providers.ci]\ntype = \"env\"\n";
    let ctx = context(dir.path(), &[("VAULT_TOKEN", TOKEN), ("KEY", "k")]);
    let providers = common::configured(workspace, "", &ctx);
    assert_eq!(common::fetch(&providers, "ci/KEY").await.unwrap(), "k");
    let err = common::fetch(&providers, "vault/kv/app/db#password")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .ends_with("vault: no address (set 'address' or VAULT_ADDR)"),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(!err.to_string().contains(TOKEN));
}
