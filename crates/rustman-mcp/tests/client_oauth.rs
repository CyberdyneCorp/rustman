//! OAuth for HTTP MCP servers against the OAuth-protected fake server of
//! `rustman-test-support`, which is also its own authorization server:
//! discovery, dynamic registration or a configured client, PKCE (S256)
//! through the loopback redirect or a pasted URL, the `resource`
//! parameter, token storage, refresh without a browser, and logout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::Resolver;
use rustman_core::{Secret, SecretStore};
use rustman_http::NO_ENVIRONMENT;
use rustman_mcp::client::oauth::{OAuthOptions, OAuthPrompt, logout, store_key};
use rustman_mcp::client::{
    ClientError, ConnectOptions, HttpDefinition, McpSession, OAuthClient, ResolvedServer,
    ServerDefinition, TransportDefinition, TrustCheck, check_trust,
};
use rustman_test_support::McpHttpServer;
use rustman_test_support::mcp_oauth::{DYNAMIC_CLIENT_ID, browse};
use tokio::sync::oneshot;

const PATIENCE: Duration = Duration::from_secs(30);

fn server(url: &str, oauth: Option<OAuthClient>) -> ResolvedServer {
    let definition = ServerDefinition {
        name: "remote".into(),
        transport: TransportDefinition::Http(HttpDefinition {
            url: url.to_owned(),
            headers: BTreeMap::new(),
            auth: None,
            oauth,
        }),
        roots: None,
        timeout_ms: None,
        source: PathBuf::from("rustman.toml"),
    };
    definition
        .resolve(&Resolver::new(), Path::new("."))
        .unwrap()
}

/// A browser: follows the authorization URL to the redirect, then either
/// delivers it to the loopback listener or hands it over as pasted.
struct Browser {
    opened: Mutex<Vec<String>>,
    paste: bool,
    pasted: Mutex<Option<oneshot::Receiver<String>>>,
    sender: Mutex<Option<oneshot::Sender<String>>>,
}

impl Browser {
    fn new(paste: bool) -> Arc<Self> {
        let (sender, receiver) = oneshot::channel();
        Arc::new(Self {
            opened: Mutex::default(),
            paste,
            pasted: Mutex::new(Some(receiver)),
            sender: Mutex::new(Some(sender)),
        })
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl OAuthPrompt for Browser {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
        let url = url.to_owned();
        let paste = self.paste;
        let sender = self.sender.lock().unwrap().take();
        tokio::spawn(async move {
            let (status, location) = browse(&url).await;
            assert_eq!(status, 302);
            let redirect = location.expect("a redirect");
            if paste {
                let _ = sender.unwrap().send(redirect);
            } else {
                browse(&redirect).await;
            }
        });
    }

    fn pasted(&self) -> BoxFuture<'static, Option<String>> {
        let receiver = self.pasted.lock().unwrap().take();
        async move {
            match receiver {
                Some(receiver) => receiver.await.ok(),
                None => None,
            }
        }
        .boxed()
    }
}

/// A prompt that must not be used.
struct NoBrowser;

impl OAuthPrompt for NoBrowser {
    fn open(&self, url: &str) {
        panic!("the browser was opened: {url}");
    }
}

fn store(dir: &Path) -> Arc<SecretStore> {
    Arc::new(SecretStore::with_keychain(dir, None))
}

async fn connect(
    server: &ResolvedServer,
    store: &Arc<SecretStore>,
    prompt: Option<Arc<dyn OAuthPrompt>>,
) -> Result<McpSession, ClientError> {
    let dir = tempfile::tempdir().unwrap();
    let trust =
        TrustStore::load_with_key(dir.path(), TrustKey::in_dir(dir.path().join("keys"))).unwrap();
    let TrustCheck::Approved(approved) = check_trust(&trust, server) else {
        panic!("HTTP servers need no approval");
    };
    let options = ConnectOptions {
        oauth: Some(OAuthOptions {
            store: store.clone(),
            prompt,
        }),
        ..ConnectOptions::default()
    };
    tokio::time::timeout(PATIENCE, McpSession::connect(server, &approved, &options))
        .await
        .expect("connected in time")
}

fn bearers(fake: &McpHttpServer) -> Vec<String> {
    fake.hits()
        .iter()
        .filter(|h| h.uri.starts_with("/mcp"))
        .filter_map(|h| h.header("authorization").map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_authorization_uses_pkce_s256_and_the_resource() {
    let fake = McpHttpServer::oauth(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let browser = Browser::new(false);
    let server = server(&fake.url(), None);
    let session = connect(&server, &store, Some(browser.clone()))
        .await
        .unwrap();
    session.ping().await.unwrap();

    let opened = browser.opened();
    assert_eq!(opened.len(), 1);
    assert!(
        opened[0].contains("code_challenge_method=S256"),
        "{opened:?}"
    );
    let params = fake.authorize_params();
    assert_eq!(params["resource"], fake.url());
    assert_eq!(params["client_id"], DYNAMIC_CLIENT_ID);
    assert_eq!(params["scope"], "mcp");
    assert_eq!(fake.registrations(), 1);
    assert_eq!(fake.grants(), ["authorization_code"]);
    let sent = bearers(&fake);
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|b| b == "Bearer access-1"), "{sent:?}");

    // The tokens are in the secret store and redacted from what the
    // session shows.
    let key = store_key("remote", &fake.url());
    let stored = store.get(NO_ENVIRONMENT, &key).unwrap().unwrap();
    assert!(stored.expose().contains("access-1"));
    let shown = session
        .redactor()
        .text("token access-1 refresh refresh-1")
        .into_owned();
    assert!(
        !shown.contains("access-1") && !shown.contains("refresh-1"),
        "{shown}"
    );
    session.shutdown().await;

    // A second connection reuses the stored token without a browser.
    let again = connect(&server, &store, Some(Arc::new(NoBrowser)))
        .await
        .unwrap();
    again.shutdown().await;
    assert_eq!(fake.grants(), ["authorization_code"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_redirect_completes_the_login_with_a_configured_client() {
    let fake = McpHttpServer::oauth(false).await;
    let dir = tempfile::tempdir().unwrap();
    let browser = Browser::new(true);
    let client = OAuthClient {
        client_id: Some("app".into()),
        ..OAuthClient::default()
    };
    let session = connect(
        &server(&fake.url(), Some(client)),
        &store(dir.path()),
        Some(browser),
    )
    .await
    .unwrap();
    assert_eq!(fake.authorize_params()["client_id"], "app");
    assert_eq!(fake.registrations(), 0);
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_registration_a_client_id_is_required() {
    let fake = McpHttpServer::oauth(false).await;
    let dir = tempfile::tempdir().unwrap();
    let err = connect(
        &server(&fake.url(), None),
        &store(dir.path()),
        Some(Arc::new(NoBrowser)),
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "mcp: oauth requires client_id for remote");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_token_is_refreshed_without_the_browser() {
    let fake = McpHttpServer::oauth(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let base = fake.url().trim_end_matches("/mcp").to_owned();
    let expired = serde_json::json!({
        "access_token": "stale-access",
        "refresh_token": "refresh-0",
        "expires_at": 1,
        "client": {
            "token_endpoint": format!("{base}/as/token"),
            "client_id": DYNAMIC_CLIENT_ID,
            "resource": fake.url()
        }
    });
    let key = store_key("remote", &fake.url());
    store
        .set(
            NO_ENVIRONMENT,
            &Secret::new(key.clone(), expired.to_string()),
        )
        .unwrap();
    fake.accept_refresh_token("refresh-0");

    let server = server(&fake.url(), None);
    let session = connect(&server, &store, Some(Arc::new(NoBrowser)))
        .await
        .unwrap();
    session.ping().await.unwrap();
    assert_eq!(fake.grants(), ["refresh_token"]);
    assert!(bearers(&fake).iter().all(|b| b == "Bearer access-1"));
    let stored = store.get(NO_ENVIRONMENT, &key).unwrap().unwrap();
    assert!(stored.expose().contains("access-1"));
    assert!(!session.redactor().text("refresh-0").contains("refresh-0"));
    session.shutdown().await;

    assert!(logout(&store, &server).unwrap());
    assert!(store.get(NO_ENVIRONMENT, &key).unwrap().is_none());
    assert!(!logout(&store, &server).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_needed_login_fails_without_a_prompt() {
    let fake = McpHttpServer::oauth(true).await;
    let dir = tempfile::tempdir().unwrap();
    let err = connect(&server(&fake.url(), None), &store(dir.path()), None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "mcp: oauth: server remote needs a login; connect from a terminal"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resource_for_another_api_is_refused_before_the_browser_opens() {
    let fake = McpHttpServer::oauth(true).await;
    fake.advertise_resource("https://api.other.example/v1");
    let dir = tempfile::tempdir().unwrap();
    let browser = Browser::new(false);
    let err = connect(
        &server(&fake.url(), None),
        &store(dir.path()),
        Some(browser.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "mcp: oauth: the protected resource https://api.other.example/v1 does not match the server URL {}",
            fake.url()
        )
    );
    assert!(browser.opened().is_empty());
    assert_eq!(fake.registrations(), 0);
    assert!(fake.grants().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authorization_server_naming_another_issuer_is_refused() {
    let fake = McpHttpServer::oauth(true).await;
    fake.advertise_issuer("https://evil.example");
    let dir = tempfile::tempdir().unwrap();
    let browser = Browser::new(false);
    let err = connect(
        &server(&fake.url(), None),
        &store(dir.path()),
        Some(browser.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("names issuer https://evil.example, not"),
        "{err}"
    );
    assert!(browser.opened().is_empty());
}
