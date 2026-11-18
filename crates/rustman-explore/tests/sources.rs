//! Task 1.3: documents from a file or a URL, with the timeout and the
//! 20 MiB cap.

mod common;

use std::time::Duration;

use rustman_explore::openapi::source::MAX_DOCUMENT_BYTES;
use rustman_explore::openapi::{FetchOptions, Location};
use rustman_explore::{ImportError, ImportOptions, import};
use rustman_test_support::TestServer;

#[tokio::test]
async fn a_document_is_imported_from_a_url() {
    let server = TestServer::http().await;
    let url = server.url("/openapi/petstore.yaml");
    let imported = import(&url, &ImportOptions::default()).await.unwrap();
    assert!(matches!(imported.location, Location::Url(_)));
    assert_eq!(imported.model.title, "URL Petstore");
    assert_eq!(
        imported.model.operations[0].operation_id.as_deref(),
        Some("listPets")
    );
}

#[tokio::test]
async fn an_oversized_url_document_names_the_limit() {
    let server = TestServer::http().await;
    let url = server.url(&format!("/bytes/{}", 25 * 1024 * 1024));
    let err = import(&url, &ImportOptions::default()).await.unwrap_err();
    assert!(matches!(err, ImportError::TooLarge { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!("{url} is larger than the 20 MiB limit")
    );
}

#[tokio::test]
async fn an_oversized_file_names_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.yaml");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();
    drop(file);
    let err = import(&path.to_string_lossy(), &ImportOptions::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string().ends_with("is larger than the 20 MiB limit"),
        "{err}"
    );
}

#[tokio::test]
async fn an_error_status_is_reported() {
    let server = TestServer::http().await;
    let err = import(&server.url("/status/404"), &ImportOptions::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string().ends_with("the server answered 404"),
        "{err}"
    );
}

#[tokio::test]
async fn a_stalled_server_times_out() {
    let server = TestServer::stalled("http").await;
    let options = ImportOptions {
        fetch: FetchOptions {
            timeout: Duration::from_millis(300),
            ..FetchOptions::default()
        },
        ..ImportOptions::default()
    };
    let err = import(&server.url("/openapi.json"), &options)
        .await
        .unwrap_err();
    assert!(err.is_request_failure(), "{err:?}");
    assert!(err.to_string().contains("timeout"), "{err}");
}

#[tokio::test]
async fn a_missing_file_is_reported() {
    let err = import("no/such/openapi.yaml", &ImportOptions::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("cannot read no/such/openapi.yaml"),
        "{err}"
    );
}

fn governed(target: &str) -> std::sync::Arc<rustman_http::GovernedSender> {
    use rustman_core::safety::{Governor, Limits, PolicyOptions, SafetyPolicy, SafetySettings};
    let settings = SafetySettings::default();
    let options = PolicyOptions {
        target,
        permission: Default::default(),
        allow_hosts: &[],
        production: None,
        environment: Default::default(),
    };
    let policy = SafetyPolicy::new(&settings, options).unwrap();
    let governor = Governor::new(Limits::from_settings(&settings));
    std::sync::Arc::new(rustman_http::GovernedSender::new(policy, governor))
}

fn through(sender: std::sync::Arc<rustman_http::GovernedSender>) -> ImportOptions {
    ImportOptions {
        fetch: FetchOptions {
            sender: Some(sender),
            ..FetchOptions::default()
        },
        ..ImportOptions::default()
    }
}

#[tokio::test]
async fn a_governed_fetch_reads_an_allowed_url() {
    let server = TestServer::http().await;
    let url = server.url("/openapi/petstore.yaml");
    let imported = import(&url, &through(governed(&url))).await.unwrap();
    assert_eq!(imported.model.title, "URL Petstore");
}

#[tokio::test]
async fn a_governed_fetch_never_follows_a_redirect_off_the_policy() {
    let allowed = TestServer::http().await;
    let elsewhere = rustman_test_support::BadApi::start().await;
    let to = elsewhere.url("/openapi.json");
    let url = allowed.url(&format!("/redirect/302?to={to}"));
    let err = import(&url, &through(governed(&url))).await.unwrap_err();
    assert!(matches!(err, ImportError::Blocked { .. }), "{err:?}");
    assert!(
        err.to_string().contains("blocked: host not allowed"),
        "{err}"
    );
    assert!(elsewhere.hits().is_empty(), "{:?}", elsewhere.hits().len());
}
