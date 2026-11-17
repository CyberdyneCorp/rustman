//! TLS verification, `ca_bundle`, insecure mode and mutual TLS (task 4.5).

mod support;

use std::path::Path;

use rustman_core::settings::Settings;
use rustman_core::vars::{ResolvedRequest, TlsFiles};
use rustman_http::{ClientAuth, HttpEngine, ProxySettings, RequestError, SendOptions, TlsSettings};
use support::request::{file, get};
use support::{TestCerts, TestServer, Tls};

fn settings(extra: &str) -> Settings {
    let text = format!("[workspace]\nname = \"t\"\nversion = 1\n{extra}");
    Settings::parse(Path::new("rustman.toml"), &text).unwrap().0
}

/// The options for `request`, connecting directly whatever proxy the
/// environment of the test process names.
fn direct_options(
    settings: &Settings,
    root: &Path,
    request: &ResolvedRequest,
) -> Result<SendOptions, RequestError> {
    let mut options = SendOptions::for_request(settings, root, request)?;
    options.proxy = ProxySettings::default();
    Ok(options)
}

async fn send(
    request: &ResolvedRequest,
    root: &Path,
    settings: &Settings,
) -> Result<u16, RequestError> {
    let options = direct_options(settings, root, request)?;
    HttpEngine::new()
        .send(request, &options)
        .await
        .map(|exchange| exchange.status)
}

#[tokio::test]
async fn a_self_signed_certificate_is_rejected_by_default() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::SelfSigned).await;
    let err = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &SendOptions::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("tls: certificate verification failed"),
        "{err}"
    );
}

#[tokio::test]
async fn the_platform_store_does_not_trust_the_test_ca() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let err = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &SendOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Tls(_)), "{err:?}");
}

#[tokio::test]
async fn a_request_ca_bundle_is_trusted() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let dir = tempfile::tempdir().unwrap();
    let mut request = get(&server.url("/status/200"));
    request.tls.ca_bundle = file(dir.path(), "dev-ca.pem", &certs.ca_pem);
    let status = send(&request, dir.path(), &settings("")).await.unwrap();
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_workspace_ca_bundle_is_trusted() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("certs")).unwrap();
    std::fs::write(dir.path().join("certs/dev-ca.pem"), &certs.ca_pem).unwrap();
    let settings = settings("[http]\nca_bundle = \"certs/dev-ca.pem\"\n");
    let status = send(&get(&server.url("/status/200")), dir.path(), &settings)
        .await
        .unwrap();
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_missing_workspace_ca_bundle_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings("[http]\nca_bundle = \"nope.pem\"\n");
    let err =
        SendOptions::for_request(&settings, dir.path(), &get("https://localhost/")).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("tls: cannot read ca_bundle nope.pem:"),
        "{err}"
    );
}

#[tokio::test]
async fn verify_tls_false_succeeds_and_is_flagged_insecure() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::SelfSigned).await;
    let mut request = get(&server.url("/status/200"));
    request.settings.verify_tls = Some(false);
    let dir = tempfile::tempdir().unwrap();
    let options = direct_options(&settings(""), dir.path(), &request).unwrap();
    let exchange = HttpEngine::new().send(&request, &options).await.unwrap();
    assert_eq!(exchange.status, 200);
    assert!(exchange.insecure);
}

#[tokio::test]
async fn every_exchange_with_verification_off_is_flagged_even_plain_http() {
    let server = TestServer::http().await;
    let mut options = SendOptions::default();
    options.tls.verify = false;
    let engine = HttpEngine::new();
    let request = get(&server.url("/status/200"));
    let exchange = engine.send(&request, &options).await.unwrap();
    assert!(exchange.insecure);
    let verified = engine
        .send(&request, &SendOptions::default())
        .await
        .unwrap();
    assert!(!verified.insecure);
}

/// A request to an mTLS server that trusts the test CA.
fn mtls_request(server: &TestServer, dir: &Path, certs: &TestCerts) -> ResolvedRequest {
    let mut request = get(&server.url("/status/200"));
    request.tls.ca_bundle = file(dir, "ca.pem", &certs.ca_pem);
    request
}

#[tokio::test]
async fn mtls_with_a_pem_certificate_and_key() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let dir = tempfile::tempdir().unwrap();
    let mut request = mtls_request(&server, dir.path(), &certs);
    request.tls.client_cert = file(dir.path(), "client.pem", &certs.client_cert_pem);
    request.tls.client_key = file(dir.path(), "client.key", &certs.client_key_pem);
    let status = send(&request, dir.path(), &settings("")).await.unwrap();
    assert_eq!(status, 200);
}

#[tokio::test]
async fn mtls_with_certificate_and_key_in_one_pem_file() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let dir = tempfile::tempdir().unwrap();
    let mut request = mtls_request(&server, dir.path(), &certs);
    let combined = format!("{}{}", certs.client_cert_pem, certs.client_key_pem);
    request.tls.client_cert = file(dir.path(), "client.pem", combined);
    let status = send(&request, dir.path(), &settings("")).await.unwrap();
    assert_eq!(status, 200);
}

#[tokio::test]
async fn mtls_with_a_pkcs12_bundle_and_passphrase() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let dir = tempfile::tempdir().unwrap();
    let mut request = mtls_request(&server, dir.path(), &certs);
    request.tls.client_pkcs12 = file(dir.path(), "client.p12", certs.client_pkcs12("p12-pass"));
    request.settings.client_pkcs12_password = Some("p12-pass".into());
    let status = send(&request, dir.path(), &settings("")).await.unwrap();
    assert_eq!(status, 200);
}

/// A workspace whose `rustman.toml [http]` holds the CA and `identity`, with
/// one request (`Probe`) to `url`; returns that request resolved.
fn workspace_request(dir: &Path, url: &str, identity: &str) -> ResolvedRequest {
    use rustman_core::Workspace;
    use rustman_core::vars::{NoSecrets, Resolver, resolve_request};
    let toml = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n[variables]\np12_password = \"ws-pass\"\n\
         [http]\nca_bundle = \"certs/ca.pem\"\n{identity}"
    );
    std::fs::write(dir.join("rustman.toml"), toml).unwrap();
    let request = format!("version: 1\nkind: http\nname: Probe\nurl: '{url}'\n");
    std::fs::create_dir_all(dir.join("api")).unwrap();
    std::fs::write(dir.join("api/probe.rman.yaml"), request).unwrap();
    let ws = Workspace::load(dir).unwrap();
    let target = ws.find_by_name("Probe").remove(0);
    let resolver = Resolver::for_request(&ws.settings, &target, None, &NoSecrets);
    resolve_request(&target, &resolver).unwrap()
}

#[tokio::test]
async fn mtls_from_the_workspace_settings() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let dir = tempfile::tempdir().unwrap();
    let certs_dir = dir.path().join("certs");
    std::fs::create_dir_all(&certs_dir).unwrap();
    std::fs::write(certs_dir.join("ca.pem"), &certs.ca_pem).unwrap();
    std::fs::write(certs_dir.join("client.pem"), &certs.client_cert_pem).unwrap();
    std::fs::write(certs_dir.join("client.key"), &certs.client_key_pem).unwrap();
    std::fs::write(certs_dir.join("client.p12"), certs.client_pkcs12("ws-pass")).unwrap();
    let url = server.url("/status/200");
    let identities = [
        "client_cert = \"certs/client.pem\"\nclient_key = \"certs/client.key\"\n",
        "client_pkcs12 = \"certs/client.p12\"\nclient_pkcs12_password = \"{{p12_password}}\"\n",
    ];
    for identity in identities {
        let request = workspace_request(dir.path(), &url, identity);
        let settings = Settings::load(dir.path()).unwrap().0;
        let status = send(&request, dir.path(), &settings).await;
        assert_eq!(status, Ok(200), "{identity}");
    }
    let request = workspace_request(dir.path(), &url, "");
    let settings = Settings::load(dir.path()).unwrap().0;
    let err = send(&request, dir.path(), &settings).await.unwrap_err();
    assert!(err.to_string().starts_with("tls:"), "{err}");
}

#[tokio::test]
async fn a_wrong_pkcs12_passphrase_is_a_tls_error() {
    let certs = TestCerts::generate();
    let dir = tempfile::tempdir().unwrap();
    let mut request = get("https://localhost/");
    request.tls.client_pkcs12 = file(dir.path(), "client.p12", certs.client_pkcs12("right"));
    request.settings.client_pkcs12_password = Some("wrong".into());
    let err = SendOptions::for_request(&settings(""), dir.path(), &request).unwrap_err();
    assert!(
        err.to_string().starts_with("tls: invalid client_pkcs12:"),
        "{err}"
    );
}

#[tokio::test]
async fn a_missing_client_certificate_is_a_tls_error() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let dir = tempfile::tempdir().unwrap();
    let request = mtls_request(&server, dir.path(), &certs);
    let err = send(&request, dir.path(), &settings("")).await.unwrap_err();
    assert!(matches!(err, RequestError::Tls(_)), "{err:?}");
    assert!(err.to_string().starts_with("tls:"), "{err}");
}

#[tokio::test]
async fn tls_settings_build_a_reusable_client_config() {
    let certs = TestCerts::generate();
    let auth = ClientAuth::from_pem(
        certs.client_cert_pem.as_bytes(),
        certs.client_key_pem.as_bytes(),
    )
    .unwrap();
    let settings = TlsSettings {
        ca_bundle: Some(certs.ca_pem.clone().into_bytes()),
        client_auth: Some(auth),
        ..TlsSettings::default()
    };
    let config = settings.client_config(&[b"h2"]).unwrap();
    assert_eq!(config.alpn_protocols, [b"h2".to_vec()]);
    assert!(config.client_auth_cert_resolver.has_certs());
    assert!(!format!("{settings:?}").contains("PRIVATE KEY"));
    let empty = TlsFiles::default();
    assert_eq!(
        TlsSettings::load(&empty, None, true).unwrap(),
        TlsSettings::default()
    );
}
