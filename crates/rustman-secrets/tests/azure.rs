//! `azure-key-vault` (secret-providers spec "Cloud providers", feature
//! `secrets-azure`) against a mock Key Vault REST server, with fake
//! `DefaultAzureCredential` sources: a service principal, workload
//! identity, managed identity and the Azure CLI.
#![cfg(feature = "secrets-azure")]

mod common;

use std::path::Path;

use common::{closed_port_url, configured, context, fetch};
use rustman_core::secrets::provider::SecretProviders;
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const VALUE: &str = "azure-secret-canary-4f17";
const ACCESS_TOKEN: &str = "azure-access-canary";
const CLIENT_SECRET: &str = "azure-client-secret-canary";

fn ok(body: Value) -> Reply {
    Reply::raw(200, "application/json", &body.to_string())
}

fn token_reply() -> Reply {
    ok(json!({"access_token": ACCESS_TOKEN, "expires_in": 3599, "token_type": "Bearer"}))
}

/// Entra ID's token endpoint, the managed identity endpoints and Key
/// Vault, on one server.
fn azure_stub(req: &RecordedRequest) -> Reply {
    let path = req.uri.split('?').next().unwrap_or_default();
    match path {
        "/tenant-1/oauth2/v2.0/token" => return entra(req),
        "/identity" if req.header("x-identity-header") == Some("app-service-header") => {
            return token_reply();
        }
        "/metadata/identity/oauth2/token" if req.header("metadata") == Some("true") => {
            return ok(json!({"access_token": ACCESS_TOKEN, "expires_in": "3599"}));
        }
        _ => {}
    }
    if req.header("authorization") != Some(&format!("Bearer {ACCESS_TOKEN}")) {
        return Reply::raw(
            401,
            "application/json",
            r#"{"error":{"code":"Unauthorized"}}"#,
        );
    }
    assert!(req.uri.ends_with("api-version=7.4"), "{}", req.uri);
    match path {
        "/secrets/db" => ok(json!({"value": json!({"password": VALUE}).to_string()})),
        "/secrets/db/v2" => ok(json!({"value": "version-2"})),
        "/secrets/locked" => Reply::raw(403, "application/json", "{}"),
        _ => Reply::raw(
            404,
            "application/json",
            r#"{"error":{"code":"SecretNotFound"}}"#,
        ),
    }
}

fn entra(req: &RecordedRequest) -> Reply {
    let form: Vec<(String, String)> = form_urlencoded::parse(&req.body).into_owned().collect();
    let has = |k: &str, v: &str| form.iter().any(|(a, b)| a == k && b == v);
    let scoped = has("grant_type", "client_credentials")
        && has("client_id", "client-1")
        && has("scope", "https://vault.azure.net/.default");
    let proof = has("client_secret", CLIENT_SECRET)
        || (has("client_assertion", "federated-jwt")
            && has(
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ));
    if scoped && proof {
        token_reply()
    } else {
        Reply::raw(401, "application/json", r#"{"error":"invalid_client"}"#)
    }
}

/// The provider `az` with the server as its vault, in an environment of
/// `vars` plus an empty `PATH` (no `az`) and no metadata service.
fn azure(server: &GraphqlHttpServer, dir: &Path, vars: &[(&str, &str)]) -> SecretProviders {
    let empty = dir.display().to_string();
    let no_imds = closed_port_url();
    let mut env = vec![
        ("PATH", empty.as_str()),
        ("AZURE_POD_IDENTITY_AUTHORITY_HOST", no_imds.as_str()),
    ];
    env.retain(|(k, _)| !vars.iter().any(|(v, _)| v == k));
    env.extend_from_slice(vars);
    let ctx = context(dir, &env);
    let ws = "[secret_providers.az]\ntype = \"azure-key-vault\"\nvault = \"my-vault\"\n";
    let url = server.url("");
    let user = format!(
        "[secret_providers.az]\nendpoint = \"{}\"\n",
        url.trim_end_matches('/')
    );
    configured(ws, &user, &ctx)
}

#[tokio::test]
async fn service_principal_from_the_environment() {
    let server = GraphqlHttpServer::start(azure_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let authority = server.url("");
    let principal = [
        ("AZURE_TENANT_ID", "tenant-1"),
        ("AZURE_CLIENT_ID", "client-1"),
        ("AZURE_CLIENT_SECRET", CLIENT_SECRET),
        ("AZURE_AUTHORITY_HOST", authority.as_str()),
    ];
    let providers = azure(&server, dir.path(), &principal);
    assert_eq!(fetch(&providers, "az/db#password").await.unwrap(), VALUE);
    assert_eq!(fetch(&providers, "az/db/v2").await.unwrap(), "version-2");
    let err = fetch(&providers, "az/nope").await.unwrap_err().to_string();
    assert_eq!(
        err,
        "unresolved secret az/nope: azure-key-vault: secret nope not found"
    );
    let err = fetch(&providers, "az/locked")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.ends_with("azure-key-vault: permission denied for locked"),
        "{err}"
    );
    assert_eq!(providers.check("az").await, Ok(()));
    let tokens = server
        .hits()
        .iter()
        .filter(|r| r.uri.contains("/token"))
        .count();
    assert_eq!(tokens, 1);
    // A wrong secret: an auth failure that never shows it.
    let mut wrong = principal;
    wrong[2] = ("AZURE_CLIENT_SECRET", "wrong-secret");
    let providers = azure(&server, dir.path(), &wrong);
    let err = providers.check("az").await.unwrap_err().to_string();
    assert_eq!(
        err,
        "azure-key-vault: authentication failed (token endpoint returned 401)"
    );
}

#[tokio::test]
async fn workload_identity() {
    let server = GraphqlHttpServer::start(azure_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("federated-token");
    std::fs::write(&file, "federated-jwt\n").unwrap();
    let file = file.display().to_string();
    let authority = server.url("");
    let providers = azure(
        &server,
        dir.path(),
        &[
            ("AZURE_TENANT_ID", "tenant-1"),
            ("AZURE_CLIENT_ID", "client-1"),
            ("AZURE_FEDERATED_TOKEN_FILE", &file),
            ("AZURE_AUTHORITY_HOST", &authority),
        ],
    );
    assert_eq!(fetch(&providers, "az/db#password").await.unwrap(), VALUE);
}

#[tokio::test]
async fn managed_identity_and_no_credentials() {
    let server = GraphqlHttpServer::start(azure_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let endpoint = server.url("/identity");
    let app_service = [
        ("IDENTITY_ENDPOINT", endpoint.as_str()),
        ("IDENTITY_HEADER", "app-service-header"),
    ];
    let providers = azure(&server, dir.path(), &app_service);
    assert_eq!(fetch(&providers, "az/db#password").await.unwrap(), VALUE);
    let imds = server.url("");
    let vm = [("AZURE_POD_IDENTITY_AUTHORITY_HOST", imds.as_str())];
    let providers = azure(&server, dir.path(), &vm);
    assert_eq!(fetch(&providers, "az/db#password").await.unwrap(), VALUE);
    let providers = azure(&server, dir.path(), &[]);
    let err = fetch(&providers, "az/db").await.unwrap_err();
    assert_eq!(
        err.error.to_string(),
        "azure-key-vault: no credentials found (DefaultAzureCredential)"
    );
    assert_eq!(err.exit_code(), 3);
}

/// The Azure CLI, faked on `PATH`: a script named `az` on Unix, and the
/// `az.cmd` shim the provider runs on Windows.
#[tokio::test]
async fn azure_cli() {
    let server = GraphqlHttpServer::start(azure_stub).await;
    let dir = tempfile::tempdir().unwrap();
    let json = format!("{{\"accessToken\": \"{ACCESS_TOKEN}\", \"expires_on\": 4102444800}}");
    if cfg!(windows) {
        let script = format!(
            "@echo off\r\nif \"%1 %2 %4\"==\"account get-access-token https://vault.azure.net\" (echo {json}& exit /b 0)\r\nexit /b 1\r\n"
        );
        std::fs::write(dir.path().join("az.cmd"), script).unwrap();
    } else {
        let az = dir.path().join("az");
        let script = format!(
            "#!/bin/sh\nif [ \"$1 $2 $4\" = \"account get-access-token https://vault.azure.net\" ]; then\n  echo '{json}'\n  exit 0\nfi\nexit 1\n"
        );
        std::fs::write(&az, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&az, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let providers = azure(&server, dir.path(), &[]);
    assert_eq!(fetch(&providers, "az/db#password").await.unwrap(), VALUE);
}
