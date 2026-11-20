//! `aws-secrets-manager` and `aws-ssm` (secret-providers spec "Cloud
//! providers", feature `secrets-aws`) against local stubs of the AWS JSON
//! APIs and of the container and instance credential endpoints. The
//! `localstack` test runs against LocalStack when `RUSTMAN_LOCALSTACK_URL`
//! is set (the CI job does).
#![cfg(feature = "secrets-aws")]

mod common;

use std::path::Path;

use common::{closed_port_url, configured, context, fetch};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const SECRET_KEY: &str = "aws-secret-key-canary-71c0";
const PASSWORD: &str = "aws-password-canary-2b9e";

fn error(code: &str) -> Reply {
    let body = json!({"__type": format!("com.amazonaws.x#{code}"), "message": "m"});
    Reply::raw(400, "application/x-amz-json-1.1", &body.to_string())
}

fn ok(body: Value) -> Reply {
    Reply::raw(200, "application/x-amz-json-1.1", &body.to_string())
}

/// Secrets Manager and SSM, answering requests signed for `access_key`.
fn aws_stub(access_key: &'static str) -> impl Fn(&RecordedRequest) -> Reply + Send + Sync {
    move |req| {
        let auth = req.header("authorization").unwrap_or_default();
        if !auth.starts_with(&format!("AWS4-HMAC-SHA256 Credential={access_key}/")) {
            return error("UnrecognizedClientException");
        }
        let body = req.json();
        match req.header("x-amz-target").unwrap_or_default() {
            "secretsmanager.GetSecretValue" => {
                assert!(
                    auth.contains("/eu-west-1/secretsmanager/aws4_request"),
                    "{auth}"
                );
                match body["SecretId"].as_str() {
                    Some("prod/db") => ok(json!({
                        "Name": "prod/db",
                        "SecretString": json!({"password": PASSWORD}).to_string(),
                    })),
                    Some("locked") => error("AccessDeniedException"),
                    _ => error("ResourceNotFoundException"),
                }
            }
            "AmazonSSM.GetParameter" => {
                assert!(auth.contains("/ssm/aws4_request"), "{auth}");
                if body["Name"] == "/app/key" && body["WithDecryption"] == true {
                    ok(json!({"Parameter": {"Name": "/app/key", "Value": "ssm-value"}}))
                } else {
                    error("ParameterNotFound")
                }
            }
            _ => error("UnknownOperationException"),
        }
    }
}

fn workspace() -> &'static str {
    r#"
[secret_providers.aws]
type = "aws-secrets-manager"
region = "eu-west-1"

[secret_providers.ssm]
type = "aws-ssm"
region = "eu-west-1"
"#
}

/// User config pointing both providers at `url`.
fn endpoints(url: &str) -> String {
    format!(
        "[secret_providers.aws]\nendpoint = \"{url}\"\n[secret_providers.ssm]\nendpoint = \"{url}\"\n"
    )
}

/// An environment with no credentials anywhere: an empty home, and the
/// instance metadata service at a closed port.
fn bare(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("HOME", home.display().to_string()),
        ("USERPROFILE", home.display().to_string()),
        ("AWS_EC2_METADATA_SERVICE_ENDPOINT", closed_port_url()),
    ]
}

fn vars<'a>(list: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    list.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// "AWS JSON secret field", plus SSM, a missing secret and a denied one;
/// no message carries the secret key.
#[tokio::test]
async fn aws_json_secret_field() {
    let server = GraphqlHttpServer::start(aws_stub("AKIDENV")).await;
    let home = tempfile::tempdir().unwrap();
    let mut env = bare(home.path());
    env.push(("AWS_ACCESS_KEY_ID", "AKIDENV".into()));
    env.push(("AWS_SECRET_ACCESS_KEY", SECRET_KEY.into()));
    let ctx = context(home.path(), &vars(&env));
    let url = server.url("");
    let providers = configured(workspace(), &endpoints(url.trim_end_matches('/')), &ctx);
    let value = fetch(&providers, "aws/prod/db#password").await.unwrap();
    assert_eq!(value, PASSWORD);
    let value = fetch(&providers, "ssm/app/key").await.unwrap();
    assert_eq!(value, "ssm-value");
    let err = fetch(&providers, "aws/prod/missing").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "unresolved secret aws/prod/missing: aws-secrets-manager: secret prod/missing not found"
    );
    assert_eq!(err.exit_code(), 3);
    let err = fetch(&providers, "aws/locked")
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "unresolved secret aws/locked: aws-secrets-manager: permission denied for locked"
    );
    let err = fetch(&providers, "ssm/nope").await.unwrap_err().to_string();
    assert!(err.ends_with("aws-ssm: secret nope not found"), "{err}");
    assert!(!err.contains(SECRET_KEY));
    assert_eq!(providers.check("aws").await, Ok(()));
}

/// "No cloud credentials": nothing in the environment, no profile, no
/// container or instance endpoint.
#[tokio::test]
async fn no_cloud_credentials() {
    let home = tempfile::tempdir().unwrap();
    let env = bare(home.path());
    let ctx = context(home.path(), &vars(&env));
    let providers = configured(workspace(), &endpoints(&closed_port_url()), &ctx);
    let err = fetch(&providers, "aws/prod/db#password").await.unwrap_err();
    assert_eq!(
        err.error.to_string(),
        "aws-secrets-manager: no credentials found in the default chain"
    );
    assert_eq!(err.exit_code(), 3);
    let err = providers.check("ssm").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "aws-ssm: no credentials found in the default chain"
    );
}

#[tokio::test]
async fn keys_come_from_the_shared_profile_files() {
    let server = GraphqlHttpServer::start(aws_stub("AKIDPROFILE")).await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".aws")).unwrap();
    std::fs::write(
        home.path().join(".aws/credentials"),
        format!("[prod]\naws_access_key_id = AKIDPROFILE\naws_secret_access_key = {SECRET_KEY}\n"),
    )
    .unwrap();
    let env = bare(home.path());
    let ctx = context(home.path(), &vars(&env));
    let url = server.url("");
    let url = url.trim_end_matches('/');
    let ws = format!(
        "{}profile = \"prod\"\n",
        workspace().replace(
            "[secret_providers.ssm]\ntype = \"aws-ssm\"\nregion = \"eu-west-1\"\n",
            ""
        )
    );
    let providers = configured(
        &ws,
        &format!("[secret_providers.aws]\nendpoint = \"{url}\"\n"),
        &ctx,
    );
    assert_eq!(
        fetch(&providers, "aws/prod/db#password").await.unwrap(),
        PASSWORD
    );
    let ws = workspace().replace(
        "region = \"eu-west-1\"\n\n",
        "region = \"eu-west-1\"\nprofile = \"missing\"\n\n",
    );
    let providers = configured(&ws, &endpoints(url), &ctx);
    let err = fetch(&providers, "aws/prod/db")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.ends_with("aws-secrets-manager: profile 'missing' not found in the shared AWS files"),
        "{err}"
    );
    // AWS_PROFILE selects the profile when none is configured.
    let mut env = bare(home.path());
    env.push(("AWS_PROFILE", "prod".into()));
    let ctx = context(home.path(), &vars(&env));
    let providers = configured(workspace(), &endpoints(url), &ctx);
    assert_eq!(fetch(&providers, "ssm/app/key").await.unwrap(), "ssm-value");
}

/// IMDSv2: a session token, the role name, then its temporary keys,
/// signed with their session token.
#[tokio::test]
async fn keys_come_from_the_instance_metadata_service() {
    let imds = GraphqlHttpServer::start(|req: &RecordedRequest| {
        let token = req.header("x-aws-ec2-metadata-token");
        match (req.method.as_str(), req.uri.as_str(), token) {
            ("PUT", "/latest/api/token", _) => Reply::raw(200, "text/plain", "imds-token"),
            ("GET", "/latest/meta-data/iam/security-credentials/", Some("imds-token")) => {
                Reply::raw(200, "text/plain", "app-role\n")
            }
            ("GET", "/latest/meta-data/iam/security-credentials/app-role", Some("imds-token")) => {
                let body = json!({
                    "AccessKeyId": "ASIAIMDS",
                    "SecretAccessKey": SECRET_KEY,
                    "Token": "imds-session-token",
                    "Expiration": "2099-01-01T00:00:00Z",
                });
                Reply::raw(200, "application/json", &body.to_string())
            }
            _ => Reply::raw(401, "text/plain", ""),
        }
    })
    .await;
    let server = GraphqlHttpServer::start(aws_stub("ASIAIMDS")).await;
    let home = tempfile::tempdir().unwrap();
    let mut env = bare(home.path());
    env.retain(|(k, _)| *k != "AWS_EC2_METADATA_SERVICE_ENDPOINT");
    env.push(("AWS_EC2_METADATA_SERVICE_ENDPOINT", imds.url("")));
    let ctx = context(home.path(), &vars(&env));
    let url = server.url("");
    let providers = configured(workspace(), &endpoints(url.trim_end_matches('/')), &ctx);
    assert_eq!(
        fetch(&providers, "aws/prod/db#password").await.unwrap(),
        PASSWORD
    );
    assert_eq!(fetch(&providers, "ssm/app/key").await.unwrap(), "ssm-value");
    let signed = server.hits();
    assert!(
        signed
            .iter()
            .all(|r| r.header("x-amz-security-token") == Some("imds-session-token"))
    );
    // The keys are held: one round of metadata calls per provider.
    let puts = imds.hits().iter().filter(|r| r.method == "PUT").count();
    assert_eq!(puts, 2);
}

/// ECS and EKS Pod Identity: the container endpoint with its token.
#[tokio::test]
async fn keys_come_from_the_container_endpoint() {
    let container = GraphqlHttpServer::start(|req: &RecordedRequest| {
        if req.header("authorization") != Some("container-token") {
            return Reply::raw(403, "application/json", "{}");
        }
        let body = json!({
            "AccessKeyId": "ASIACONTAINER",
            "SecretAccessKey": SECRET_KEY,
            "Token": "container-session",
        });
        Reply::raw(200, "application/json", &body.to_string())
    })
    .await;
    let server = GraphqlHttpServer::start(aws_stub("ASIACONTAINER")).await;
    let home = tempfile::tempdir().unwrap();
    let token_file = home.path().join("token");
    std::fs::write(&token_file, "container-token\n").unwrap();
    let mut env = bare(home.path());
    env.push((
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        container.url("/creds"),
    ));
    env.push((
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        token_file.display().to_string(),
    ));
    let ctx = context(home.path(), &vars(&env));
    let url = server.url("");
    let providers = configured(workspace(), &endpoints(url.trim_end_matches('/')), &ctx);
    assert_eq!(
        fetch(&providers, "aws/prod/db#password").await.unwrap(),
        PASSWORD
    );
}

/// `secret test`: rejected keys are an auth failure; a missing probe
/// secret means the keys were accepted.
#[tokio::test]
async fn health_check_tells_rejected_keys() {
    let server = GraphqlHttpServer::start(aws_stub("AKIDGOOD")).await;
    let home = tempfile::tempdir().unwrap();
    let url = server.url("");
    let url = url.trim_end_matches('/');
    for (key, expected) in [
        ("AKIDGOOD", Ok(())),
        (
            "AKIDBAD",
            Err("aws-secrets-manager: authentication failed".to_owned()),
        ),
    ] {
        let mut env = bare(home.path());
        env.push(("AWS_ACCESS_KEY_ID", key.into()));
        env.push(("AWS_SECRET_ACCESS_KEY", SECRET_KEY.into()));
        let ctx = context(home.path(), &vars(&env));
        let providers = configured(workspace(), &endpoints(url), &ctx);
        let checked = providers.check("aws").await.map_err(|e| e.to_string());
        assert_eq!(checked, expected);
    }
}

#[tokio::test]
async fn a_region_is_required() {
    let home = tempfile::tempdir().unwrap();
    let mut env = bare(home.path());
    env.push(("AWS_ACCESS_KEY_ID", "AKID".into()));
    env.push(("AWS_SECRET_ACCESS_KEY", SECRET_KEY.into()));
    let ctx = context(home.path(), &vars(&env));
    let ws = "[secret_providers.aws]\ntype = \"aws-secrets-manager\"\n";
    let providers = configured(ws, "", &ctx);
    let err = fetch(&providers, "aws/x").await.unwrap_err();
    assert_eq!(
        err.error.to_string(),
        "aws-secrets-manager: no region (set 'region' or AWS_REGION)"
    );
    assert_eq!(err.exit_code(), 2);
}

/// A workspace `region` becomes part of the endpoint host: anything but a
/// region name is refused before signed keys go anywhere.
#[tokio::test]
async fn a_region_cannot_change_the_endpoint_host() {
    let server = GraphqlHttpServer::start(aws_stub("AKID")).await;
    let home = tempfile::tempdir().unwrap();
    let mut env = bare(home.path());
    env.push(("AWS_ACCESS_KEY_ID", "AKID".into()));
    env.push(("AWS_SECRET_ACCESS_KEY", SECRET_KEY.into()));
    let ctx = context(home.path(), &vars(&env));
    let authority = server.url("");
    let authority = authority
        .trim_start_matches("http://")
        .trim_end_matches('/');
    for region in [
        format!("x@{authority}/"),
        format!("x.{authority}#"),
        "EU-WEST-1".to_owned(),
    ] {
        let ws = format!(
            "[secret_providers.aws]\ntype = \"aws-secrets-manager\"\nregion = \"{region}\"\n"
        );
        let providers = configured(&ws, "", &ctx);
        let err = fetch(&providers, "aws/prod/db").await.unwrap_err();
        assert_eq!(
            err.error.to_string(),
            format!(
                "aws-secrets-manager: invalid region '{region}' (expected letters, digits and '-')"
            )
        );
        assert_eq!(err.exit_code(), 2);
    }
    assert!(server.hits().is_empty());
}

/// Task 4.1 against LocalStack: the CI job creates `prod/db` and
/// `/app/key` there first.
#[tokio::test]
async fn localstack() {
    let Ok(url) = std::env::var("RUSTMAN_LOCALSTACK_URL") else {
        eprintln!("skipped: RUSTMAN_LOCALSTACK_URL is not set (the CI LocalStack job sets it)");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let mut env = bare(home.path());
    env.push(("AWS_ACCESS_KEY_ID", "test".into()));
    env.push(("AWS_SECRET_ACCESS_KEY", "test".into()));
    let ctx = context(home.path(), &vars(&env));
    let providers = configured(workspace(), &endpoints(url.trim_end_matches('/')), &ctx);
    let value = fetch(&providers, "aws/prod/db#password").await.unwrap();
    assert_eq!(value, "localstack-password");
    assert_eq!(
        fetch(&providers, "ssm/app/key").await.unwrap(),
        "localstack-parameter"
    );
    let err = fetch(&providers, "aws/prod/missing").await.unwrap_err();
    assert!(err.to_string().ends_with("not found"), "{err}");
    assert_eq!(providers.check("aws").await, Ok(()));
}
