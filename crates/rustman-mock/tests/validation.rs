//! Tasks 3.1 and 3.2: request validation (400, 415, 422) and optional
//! security enforcement (401 with `WWW-Authenticate`).

mod common;

use common::{
    config, get, header, json, post_json, send, start, start_fixture, start_with, status,
};
use rustman_core::Secret;
use rustman_mock::MockConfig;
use serde_json::json;

#[tokio::test]
async fn missing_required_query_parameter() {
    let mock = start_fixture("store.yaml", None, config()).await;
    let response = get(mock.local_addr(), "/search", &[]).await;
    assert_eq!(status(&response), 400, "{response}");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    let errors = &json(&response)["errors"];
    assert!(
        errors
            .as_array()
            .unwrap()
            .contains(&json!({"location": "query", "pointer": "/q", "message": "required"})),
        "{errors}"
    );
    let typed = get(mock.local_addr(), "/search?q=x&limit=500", &[]).await;
    assert_eq!(status(&typed), 400, "{typed}");
    assert_eq!(json(&typed)["errors"][0]["pointer"], "/limit");
    assert_eq!(
        status(&get(mock.local_addr(), "/search?q=x&limit=5", &[]).await),
        200
    );
    mock.stop().await;
}

#[tokio::test]
async fn schema_violation() {
    let mock = start().await;
    let response = post_json(mock.local_addr(), "POST", "/pets", r#"{"name": 5}"#).await;
    assert_eq!(status(&response), 422, "{response}");
    let body = json(&response);
    assert_eq!(body["errors"][0]["location"], "body");
    assert_eq!(body["errors"][0]["pointer"], "/name");
    let ok = post_json(mock.local_addr(), "POST", "/pets", r#"{"name": "Rex"}"#).await;
    assert_eq!(status(&ok), 201, "{ok}");
    mock.stop().await;
}

#[tokio::test]
async fn unsupported_media_type() {
    let mock = start().await;
    let plain = ["Content-Type: text/plain"];
    let response = send(mock.local_addr(), "POST", "/pets", &plain, "Rex").await;
    assert_eq!(status(&response), 415, "{response}");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    mock.stop().await;
}

#[tokio::test]
async fn invalid_path_parameter() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/pets/abc", &[]).await;
    assert_eq!(status(&response), 400, "{response}");
    assert_eq!(json(&response)["errors"][0]["location"], "path");
    mock.stop().await;
}

#[tokio::test]
async fn no_validate_skips_validation() {
    let mock = start_with(MockConfig {
        validate: false,
        ..config()
    })
    .await;
    let addr = mock.local_addr();
    assert_eq!(
        status(&post_json(addr, "POST", "/pets", r#"{"name": 5}"#).await),
        201
    );
    let plain = ["Content-Type: text/plain"];
    assert_eq!(
        status(&send(addr, "POST", "/pets", &plain, "Rex").await),
        201
    );
    mock.stop().await;
}

fn enforcing() -> MockConfig {
    MockConfig {
        enforce_security: true,
        ..config()
    }
}

#[tokio::test]
async fn missing_bearer_token() {
    let mock = start_fixture("store.yaml", None, enforcing()).await;
    let addr = mock.local_addr();
    let response = get(addr, "/users", &[]).await;
    assert_eq!(status(&response), 401, "{response}");
    assert_eq!(header(&response, "www-authenticate"), Some("Bearer"));
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    assert_eq!(
        status(&get(addr, "/users", &["Authorization: Bearer abc"]).await),
        200
    );
    assert_eq!(status(&get(addr, "/search?q=1", &[]).await), 200);
    mock.stop().await;
}

#[tokio::test]
async fn not_enforced_by_default() {
    let mock = start_fixture("store.yaml", None, config()).await;
    let response = get(mock.local_addr(), "/users", &[]).await;
    assert_eq!(status(&response), 200, "{response}");
    mock.stop().await;
}

#[tokio::test]
async fn basic_or_api_key_alternatives() {
    let mock = start_fixture("store.yaml", None, enforcing()).await;
    let addr = mock.local_addr();
    let response = get(addr, "/admin", &[]).await;
    assert_eq!(status(&response), 401);
    assert!(
        response.contains("www-authenticate: Basic realm=\"rustman-mock\""),
        "{response}"
    );
    assert!(
        response.contains("www-authenticate: ApiKey name=\"X-API-Key\""),
        "{response}"
    );
    assert_eq!(status(&get(addr, "/admin", &["X-API-Key: k"]).await), 200);
    assert_eq!(
        status(&get(addr, "/admin", &["Authorization: Basic YTpi"]).await),
        200
    );
    mock.stop().await;
}

#[tokio::test]
async fn expected_values_are_checked_when_configured() {
    let mut config = enforcing();
    config.security.insert(
        "bearerAuth".into(),
        Secret::new("api_token", "s3cret-token".to_owned()),
    );
    let mock = start_fixture("store.yaml", None, config).await;
    let addr = mock.local_addr();
    let wrong = get(addr, "/users", &["Authorization: Bearer nope"]).await;
    assert_eq!(status(&wrong), 401, "{wrong}");
    assert!(!wrong.contains("s3cret-token"), "{wrong}");
    let right = get(addr, "/users", &["Authorization: Bearer s3cret-token"]).await;
    assert_eq!(status(&right), 200, "{right}");
    mock.stop().await;
}

#[tokio::test]
async fn security_comes_before_validation() {
    let mock = start_fixture("store.yaml", None, enforcing()).await;
    let response = post_json(mock.local_addr(), "POST", "/users", r#"{"name": 5}"#).await;
    assert_eq!(status(&response), 401, "{response}");
    mock.stop().await;
}
