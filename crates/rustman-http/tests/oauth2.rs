//! OAuth2 client credentials and authorization code + PKCE (task 4.9).

mod support;

use std::sync::{Arc, Mutex};

use rustman_core::model::{Auth, OAuth2, OAuth2Grant};
use rustman_core::vars::ResolvedRequest;
use rustman_core::{Redactor, Secret, SecretStore};
use rustman_http::{HttpEngine, Login, OAuth2Context, RequestError, SendOptions};
use support::OAuthServer;
use support::oauth::{CLIENT_ID, CLIENT_SECRET};
use support::request::get;

fn config(server: &OAuthServer, grant: OAuth2Grant) -> OAuth2 {
    OAuth2 {
        grant,
        token_url: server.url("/token"),
        auth_url: Some(server.url("/authorize")),
        client_id: CLIENT_ID.into(),
        client_secret: Some(CLIENT_SECRET.into()),
        scopes: Some(vec!["read".into()]),
    }
}

fn api_request(server: &OAuthServer, config: OAuth2) -> ResolvedRequest {
    let mut request = get(&server.url("/api"));
    request.auth = Some(Auth::OAuth2(config));
    request
}

fn context<'a>(store: &'a SecretStore, login: Login<'a>) -> OAuth2Context<'a> {
    OAuth2Context {
        store,
        env: Some("dev"),
        cache_key: "api/me".into(),
        login,
    }
}

fn non_interactive() -> Login<'static> {
    Login::NonInteractive {
        request: "api/me".into(),
    }
}

async fn send(
    engine: &HttpEngine,
    server: &OAuthServer,
    config: &OAuth2,
    context: &OAuth2Context<'_>,
) -> Result<(u16, String), RequestError> {
    let mut request = api_request(server, config.clone());
    let exchange = engine
        .send_authorized(&mut request, &SendOptions::default(), context)
        .await?;
    let body = String::from_utf8(exchange.body.to_vec()).unwrap();
    Ok((exchange.status, body))
}

#[tokio::test]
async fn client_credentials_tokens_are_cached() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::ClientCredentials);
    let ctx = context(&store, non_interactive());
    let first = send(&HttpEngine::new(), &server, &config, &ctx)
        .await
        .unwrap();
    assert_eq!(first, (200, "{\"token\":\"tok-1\"}".into()));
    // A second request, from a new engine as in a later run: the cached
    // token (expires_in 3600) is reused.
    let second = send(&HttpEngine::new(), &server, &config, &ctx)
        .await
        .unwrap();
    assert_eq!(second.0, 200);
    assert_eq!(server.grants(), ["client_credentials"]);
}

#[tokio::test]
async fn the_token_call_does_not_follow_redirects() {
    // A 307 from the token URL would re-post the form, client secret
    // included, to wherever `Location` points.
    let server = OAuthServer::start().await;
    let redirector = support::TestServer::http().await;
    let to: String = form_urlencoded::byte_serialize(server.url("/token").as_bytes()).collect();
    let mut config = config(&server, OAuth2Grant::ClientCredentials);
    config.token_url = redirector.url(&format!("/redirect/307?to={to}"));
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let ctx = context(&store, non_interactive());
    let err = send(&HttpEngine::new(), &server, &config, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Auth(_)), "{err}");
    assert!(server.grants().is_empty(), "{:?}", server.grants());
}

#[tokio::test]
async fn caches_are_per_environment() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::ClientCredentials);
    let engine = HttpEngine::new();
    send(
        &engine,
        &server,
        &config,
        &context(&store, non_interactive()),
    )
    .await
    .unwrap();
    let prod = OAuth2Context {
        env: Some("prod"),
        ..context(&store, non_interactive())
    };
    send(&engine, &server, &config, &prod).await.unwrap();
    assert_eq!(server.grants().len(), 2);
}

#[tokio::test]
async fn a_401_reacquires_the_token_once() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::ClientCredentials);
    let ctx = context(&store, non_interactive());
    let engine = HttpEngine::new();
    send(&engine, &server, &config, &ctx).await.unwrap();
    server.revoke_access_tokens();
    let (status, body) = send(&engine, &server, &config, &ctx).await.unwrap();
    assert_eq!((status, body.as_str()), (200, "{\"token\":\"tok-2\"}"));
    assert_eq!(
        server.grants(),
        ["client_credentials", "client_credentials"]
    );
}

#[tokio::test]
async fn a_bad_client_secret_is_an_auth_error() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let mut config = config(&server, OAuth2Grant::ClientCredentials);
    config.client_secret = Some("wrong-secret".into());
    let mut request = api_request(&server, config);
    request
        .secrets
        .push(Secret::new("client_secret", "wrong-secret".to_owned()));
    let err = HttpEngine::new()
        .send_authorized(
            &mut request,
            &SendOptions::default(),
            &context(&store, non_interactive()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Auth(_)));
    assert_eq!(
        err.to_string(),
        "oauth2: token endpoint returned 401 (invalid_client)"
    );
}

#[tokio::test]
async fn authorization_code_needs_a_terminal() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::AuthorizationCode);
    let err = send(
        &HttpEngine::new(),
        &server,
        &config,
        &context(&store, non_interactive()),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "oauth2: interactive login required (run 'rustman auth login api/me')"
    );
    assert!(server.grants().is_empty());
}

/// A stand-in for the browser: opens the authorization URL, whose redirect
/// leads to the loopback listener. Records the URL it was given.
fn browser(opened: Arc<Mutex<Vec<String>>>) -> impl Fn(&str) + Send + Sync {
    move |url: &str| {
        opened.lock().unwrap().push(url.to_owned());
        let request = get(url);
        tokio::spawn(async move {
            HttpEngine::new()
                .send(&request, &SendOptions::default())
                .await
                .unwrap();
        });
    }
}

#[tokio::test]
async fn authorization_code_uses_pkce_s256_over_loopback() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::AuthorizationCode);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let open = browser(opened.clone());
    let ctx = context(&store, Login::Interactive(&open));
    let (status, _) = send(&HttpEngine::new(), &server, &config, &ctx)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(opened.lock().unwrap().len(), 1);
    let params = server.authorize_params();
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["scope"], "read");
    assert_eq!(params["code_challenge"].len(), 43);
    assert!(
        params["redirect_uri"].starts_with("http://127.0.0.1:"),
        "{}",
        params["redirect_uri"]
    );
    // The server checked the verifier against the challenge.
    assert_eq!(server.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn expired_tokens_are_refreshed_without_a_new_login() {
    let server = OAuthServer::start().await;
    server.set_expires_in(1);
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::AuthorizationCode);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let open = browser(opened.clone());
    let engine = HttpEngine::new();
    send(
        &engine,
        &server,
        &config,
        &context(&store, Login::Interactive(&open)),
    )
    .await
    .unwrap();
    // The token expires within the safety margin, so the next request
    // refreshes it, even without a terminal.
    let (status, body) = send(
        &engine,
        &server,
        &config,
        &context(&store, non_interactive()),
    )
    .await
    .unwrap();
    assert_eq!((status, body.as_str()), (200, "{\"token\":\"tok-2\"}"));
    assert_eq!(server.grants(), ["authorization_code", "refresh_token"]);
    assert_eq!(opened.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn login_replaces_the_cached_token() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::AuthorizationCode);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let open = browser(opened.clone());
    let engine = HttpEngine::new();
    let request = api_request(&server, config.clone());
    let interactive = context(&store, Login::Interactive(&open));
    for _ in 0..2 {
        engine
            .oauth2_login(&request, &SendOptions::default(), &interactive)
            .await
            .unwrap();
    }
    assert_eq!(
        server.grants(),
        ["authorization_code", "authorization_code"]
    );
    // The stored token now serves requests without a terminal.
    let (status, body) = send(
        &engine,
        &server,
        &config,
        &context(&store, non_interactive()),
    )
    .await
    .unwrap();
    assert_eq!((status, body.as_str()), (200, "{\"token\":\"tok-2\"}"));
}

#[tokio::test]
async fn login_requires_oauth2_auth() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let err = HttpEngine::new()
        .oauth2_login(
            &get("http://127.0.0.1:1/"),
            &SendOptions::default(),
            &context(&store, non_interactive()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "oauth2: the request does not use oauth2 auth"
    );
}

#[tokio::test]
async fn tokens_are_redacted_from_every_sink() {
    let server = OAuthServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let config = config(&server, OAuth2Grant::ClientCredentials);
    let mut request = api_request(&server, config);
    let exchange = HttpEngine::new()
        .send_authorized(
            &mut request,
            &SendOptions::default(),
            &context(&store, non_interactive()),
        )
        .await
        .unwrap();
    let body = String::from_utf8(exchange.body.to_vec()).unwrap();
    assert!(body.contains("tok-1"));
    let shown = Redactor::for_request(&request).text(&body).into_owned();
    assert_eq!(shown, "{\"token\":\"«redacted:oauth2_access_token»\"}");
    let secrets = std::fs::read_to_string(dir.path().join(".rustman/secrets.yaml")).unwrap();
    assert!(secrets.contains("oauth2:api/me"), "{secrets}");
}
