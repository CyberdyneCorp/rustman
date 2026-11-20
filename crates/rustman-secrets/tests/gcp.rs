//! `gcp-secret-manager` (secret-providers spec "Cloud providers", feature
//! `secrets-gcp`) against a mock Secret Manager REST server, with fake
//! Application Default Credentials: a service account key, gcloud's user
//! credentials and the metadata server.
#![cfg(feature = "secrets-gcp")]

mod common;

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{closed_port_url, configured, context, fetch};
use rustman_core::secrets::provider::SecretProviders;
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const VALUE: &str = "gcp-secret-canary-9d41";
const ACCESS_TOKEN: &str = "ya29.gcp-access-canary";
const REFRESH_TOKEN: &str = "gcp-refresh-canary-11aa";

fn ok(body: Value) -> Reply {
    Reply::raw(200, "application/json", &body.to_string())
}

/// The token endpoint and Secret Manager, on one server.
fn google_stub(req: &RecordedRequest) -> Reply {
    if req.uri == "/token" {
        return token(req);
    }
    if req.header("authorization") != Some(&format!("Bearer {ACCESS_TOKEN}")) {
        return Reply::raw(401, "application/json", r#"{"error":{"code":401}}"#);
    }
    match req.uri.as_str() {
        "/v1/projects/my-project/secrets/db/versions/latest:access" => ok(
            json!({"payload": {"data": STANDARD.encode(json!({"password": VALUE}).to_string())}}),
        ),
        "/v1/projects/my-project/secrets/db/versions/3:access" => {
            ok(json!({"payload": {"data": STANDARD.encode("v3")}}))
        }
        "/v1/projects/my-project/secrets/locked/versions/latest:access" => {
            Reply::raw(403, "application/json", "{}")
        }
        _ => Reply::raw(404, "application/json", r#"{"error":{"code":404}}"#),
    }
}

/// A service account's signed assertion, or a user's refresh token.
fn token(req: &RecordedRequest) -> Reply {
    let form: Vec<(String, String)> = form_urlencoded::parse(&req.body).into_owned().collect();
    let field = |name: &str| {
        form.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let valid = match field("grant_type") {
        Some("urn:ietf:params:oauth:grant-type:jwt-bearer") => {
            let assertion = field("assertion").unwrap_or_default();
            let claims = assertion.split('.').nth(1).unwrap_or_default();
            let claims: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
            claims["iss"] == "sa@my-project.iam.gserviceaccount.com"
                && claims["scope"] == "https://www.googleapis.com/auth/cloud-platform"
        }
        Some("refresh_token") => {
            field("refresh_token") == Some(REFRESH_TOKEN) && field("client_id") == Some("cid")
        }
        _ => false,
    };
    if valid {
        ok(json!({"access_token": ACCESS_TOKEN, "expires_in": 3599, "token_type": "Bearer"}))
    } else {
        Reply::raw(400, "application/json", r#"{"error":"invalid_grant"}"#)
    }
}

fn rsa_key() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rustman-http/tests/fixtures/jwt/rsa.pem");
    std::fs::read_to_string(path).unwrap()
}

/// The provider `gcp`, with the server as its endpoint, in an environment
/// of `vars` (plus an empty home and no metadata server).
fn gcp(
    server: &GraphqlHttpServer,
    home: &Path,
    vars: &[(&str, &str)],
    project: &str,
) -> SecretProviders {
    let home_text = home.display().to_string();
    let no_metadata = closed_port_url();
    let mut env = vec![
        ("HOME", home_text.as_str()),
        ("APPDATA", home_text.as_str()),
        (
            "GCE_METADATA_HOST",
            no_metadata.trim_start_matches("http://"),
        ),
    ];
    env.retain(|(k, _)| !vars.iter().any(|(v, _)| v == k));
    env.extend_from_slice(vars);
    let ctx = context(home, &env);
    let ws = format!("[secret_providers.gcp]\ntype = \"gcp-secret-manager\"\n{project}");
    let url = server.url("");
    let user = format!(
        "[secret_providers.gcp]\nendpoint = \"{}\"\n",
        url.trim_end_matches('/')
    );
    configured(&ws, &user, &ctx)
}

#[tokio::test]
async fn service_account_key() {
    let server = GraphqlHttpServer::start(google_stub).await;
    let home = tempfile::tempdir().unwrap();
    let key_file = home.path().join("sa.json");
    let key = json!({
        "type": "service_account",
        "project_id": "my-project",
        "client_email": "sa@my-project.iam.gserviceaccount.com",
        "private_key": rsa_key(),
        "token_uri": server.url("/token"),
    });
    std::fs::write(&key_file, key.to_string()).unwrap();
    let file = key_file.display().to_string();
    let providers = gcp(
        &server,
        home.path(),
        &[("GOOGLE_APPLICATION_CREDENTIALS", &file)],
        "",
    );
    // The project comes from the key file.
    assert_eq!(fetch(&providers, "gcp/db#password").await.unwrap(), VALUE);
    assert_eq!(fetch(&providers, "gcp/db@3").await.unwrap(), "v3");
    let err = fetch(&providers, "gcp/nope").await.unwrap_err().to_string();
    assert_eq!(
        err,
        "unresolved secret gcp/nope: gcp-secret-manager: secret nope not found"
    );
    let err = fetch(&providers, "gcp/locked")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.ends_with("gcp-secret-manager: permission denied for locked"),
        "{err}"
    );
    assert_eq!(providers.check("gcp").await, Ok(()));
    // One token for every call.
    let tokens = server.hits().iter().filter(|r| r.uri == "/token").count();
    assert_eq!(tokens, 1);
}

#[tokio::test]
async fn gcloud_user_credentials() {
    let server = GraphqlHttpServer::start(google_stub).await;
    let home = tempfile::tempdir().unwrap();
    let dir = if cfg!(windows) {
        home.path().join("gcloud")
    } else {
        home.path().join(".config/gcloud")
    };
    std::fs::create_dir_all(&dir).unwrap();
    let credentials = json!({
        "type": "authorized_user",
        "client_id": "cid",
        "client_secret": "csecret",
        "refresh_token": REFRESH_TOKEN,
        "token_uri": server.url("/token"),
    });
    std::fs::write(
        dir.join("application_default_credentials.json"),
        credentials.to_string(),
    )
    .unwrap();
    let project = "project = \"my-project\"\n";
    let providers = gcp(&server, home.path(), &[], project);
    assert_eq!(fetch(&providers, "gcp/db#password").await.unwrap(), VALUE);
    // A revoked refresh token: an auth failure that shows no credential.
    let revoked = json!({
        "type": "authorized_user", "client_id": "cid", "client_secret": "csecret",
        "refresh_token": "revoked-refresh", "token_uri": server.url("/token"),
    });
    std::fs::write(
        dir.join("application_default_credentials.json"),
        revoked.to_string(),
    )
    .unwrap();
    let providers = gcp(&server, home.path(), &[], project);
    let err = providers.check("gcp").await.unwrap_err().to_string();
    assert_eq!(
        err,
        "gcp-secret-manager: authentication failed (token endpoint returned 400)"
    );
}

#[tokio::test]
async fn metadata_server_and_no_credentials() {
    let server = GraphqlHttpServer::start(google_stub).await;
    let metadata = GraphqlHttpServer::start(|req: &RecordedRequest| {
        let path = "/computeMetadata/v1/instance/service-accounts/default/token";
        if req.uri == path && req.header("metadata-flavor") == Some("Google") {
            ok(json!({"access_token": ACCESS_TOKEN, "expires_in": 3000}))
        } else {
            Reply::raw(404, "text/plain", "")
        }
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    let host = metadata.url("");
    let host = host.trim_start_matches("http://").trim_end_matches('/');
    let project = "project = \"my-project\"\n";
    let providers = gcp(
        &server,
        home.path(),
        &[("GCE_METADATA_HOST", host)],
        project,
    );
    assert_eq!(fetch(&providers, "gcp/db#password").await.unwrap(), VALUE);
    let providers = gcp(&server, home.path(), &[], project);
    let err = fetch(&providers, "gcp/db").await.unwrap_err();
    assert_eq!(
        err.error.to_string(),
        "gcp-secret-manager: no credentials found (Application Default Credentials)"
    );
    assert_eq!(err.exit_code(), 3);
    let providers = gcp(&server, home.path(), &[], "");
    let err = fetch(&providers, "gcp/db").await.unwrap_err();
    assert_eq!(
        err.error.to_string(),
        "gcp-secret-manager: no project (set 'project' or GOOGLE_CLOUD_PROJECT)"
    );
}
