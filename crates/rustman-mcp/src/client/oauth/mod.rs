//! OAuth 2.1 for HTTP MCP servers (mcp-client spec "OAuth 2.1 for HTTP
//! servers", "OAuth token storage", design D5). On a `401` the client
//! discovers the authorization server (RFC 9728, RFC 8414), registers
//! itself when the server offers dynamic registration (RFC 7591) or uses
//! the definition's `client_id`, and runs the authorization-code flow with
//! PKCE (S256) through the system browser and a loopback redirect (or a
//! pasted redirect URL), with the `resource` parameter (RFC 8707). Tokens
//! live in the secret store, are refreshed before they expire, and every
//! value is added to the session's redactor.

mod browser;
mod discovery;
mod token;
mod web;

use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rustman_core::{Secret, SecretStore};
use rustman_http::{HttpEngine, LOGIN_TIMEOUT, Loopback, Pkce, SendOptions};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use url::Url;

use super::error::ClientError;
use super::resolve::{ResolvedOAuthClient, ResolvedServer, ResolvedTransport};
use super::shared::SharedRedactor;
pub use browser::open_browser;
use discovery::{ResourceMetadata, ServerMetadata};
pub(crate) use token::ACCESS_TOKEN;
pub use token::store_key;
use token::{Client, Grant, TokenStore, now};
use web::Web;

/// How the user takes part in a login.
pub trait OAuthPrompt: Send + Sync {
    /// Show the authorization URL: open the browser and print it.
    fn open(&self, url: &str);

    /// The redirect URL, pasted by a user whose browser cannot reach the
    /// loopback listener (over SSH, say). `None`: only the listener counts.
    fn pasted(&self) -> BoxFuture<'static, Option<String>> {
        std::future::pending().boxed()
    }
}

/// Where tokens are kept and how to log in.
#[derive(Clone)]
pub struct OAuthOptions {
    pub store: Arc<SecretStore>,
    /// `None` without a user to ask: a needed login fails.
    pub prompt: Option<Arc<dyn OAuthPrompt>>,
}

/// Delete the stored tokens of `server` (`rustman mcp-client logout`).
/// Returns whether there were any.
pub fn logout(store: &SecretStore, server: &ResolvedServer) -> Result<bool, ClientError> {
    let ResolvedTransport::Http(http) = &server.transport else {
        return Ok(false);
    };
    store
        .remove(
            rustman_http::NO_ENVIRONMENT,
            &store_key(&server.name, &http.url.text),
        )
        .map_err(|e| ClientError::OAuth(format!("token store: {e}")))
}

struct Tokens {
    loaded: bool,
    grant: Option<Grant>,
}

/// The OAuth state of one HTTP session.
pub(crate) struct Authorizer {
    server: String,
    url: String,
    config: ResolvedOAuthClient,
    web: Web,
    store: TokenStore,
    prompt: Option<Arc<dyn OAuthPrompt>>,
    tokens: Mutex<Tokens>,
    redactor: SharedRedactor,
}

impl Authorizer {
    pub fn new(
        server: &str,
        url: &str,
        config: ResolvedOAuthClient,
        options: &OAuthOptions,
        engine: Arc<HttpEngine>,
        send: &SendOptions,
        redactor: SharedRedactor,
    ) -> Self {
        Self {
            server: server.to_owned(),
            url: url.to_owned(),
            config,
            web: Web::new(engine, send),
            store: TokenStore {
                store: options.store.clone(),
                key: store_key(server, url),
            },
            prompt: options.prompt.clone(),
            tokens: Mutex::new(Tokens {
                loaded: false,
                grant: None,
            }),
            redactor,
        }
    }

    fn error(&self, message: &str) -> ClientError {
        ClientError::OAuth(self.redactor.text(message))
    }

    fn learn(&self, grant: &Grant) {
        for secret in grant.secrets() {
            self.redactor.add(&secret);
        }
    }

    /// The access token to send: the stored one, refreshed first when it is
    /// about to expire. `None` until a login. A failed refresh drops the
    /// tokens, so the server's next `401` starts a login.
    pub async fn bearer(&self) -> Result<Option<Secret<String>>, ClientError> {
        let mut tokens = self.tokens.lock().await;
        if !tokens.loaded {
            tokens.grant = self.store.load().map_err(|e| self.error(&e))?;
            tokens.loaded = true;
            if let Some(grant) = &tokens.grant {
                self.learn(grant);
            }
        }
        let Some(grant) = tokens.grant.clone() else {
            return Ok(None);
        };
        if grant.is_fresh(now()) {
            return Ok(Some(grant.access()));
        }
        match token::refresh(&self.web, &grant).await {
            Ok(renewed) => {
                self.learn(&renewed);
                self.store.save(&renewed).map_err(|e| self.error(&e))?;
                let access = renewed.access();
                tokens.grant = Some(renewed);
                Ok(Some(access))
            }
            Err(_) => {
                let _ = self.store.remove();
                tokens.grant = None;
                Ok(None)
            }
        }
    }

    /// Log in after the server answered `401` with `challenge`.
    pub async fn login(&self, challenge: Option<&str>) -> Result<(), ClientError> {
        let Some(prompt) = self.prompt.clone() else {
            return Err(self.error(&format!(
                "server {} needs a login; connect from a terminal",
                self.server
            )));
        };
        let (resource, server) = self.discover(challenge).await?;
        let loopback = Loopback::bind()
            .await
            .map_err(|e| self.error(&e.to_string()))?;
        let redirect = loopback.redirect_uri().to_owned();
        let client = self.client(&server, &resource, &redirect).await?;
        for secret in client.secrets() {
            self.redactor.add(&secret);
        }
        let pkce = Pkce::new().map_err(|e| self.error(&e.to_string()))?;
        let scopes = self.scopes(&resource);
        prompt.open(&authorization_url(
            &server, &client, &redirect, &pkce, &scopes,
        )?);
        let code = wait_for_code(&loopback, &pkce.state, prompt.pasted())
            .await
            .map_err(|e| self.error(&e))?;
        self.redactor.add(&Secret::new("oauth_code", code.clone()));
        let params = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", pkce.verifier),
        ];
        let grant = token::request_token(&self.web, &client, params)
            .await
            .map_err(|e| self.error(&e))?;
        self.learn(&grant);
        self.store.save(&grant).map_err(|e| self.error(&e))?;
        let mut tokens = self.tokens.lock().await;
        tokens.loaded = true;
        tokens.grant = Some(grant);
        Ok(())
    }

    async fn discover(
        &self,
        challenge: Option<&str>,
    ) -> Result<(ResourceMetadata, ServerMetadata), ClientError> {
        let resource = discovery::resource(&self.web, challenge, &self.url)
            .await
            .map_err(|e| self.error(&e))?;
        discovery::checked_resource(&resource, &self.url).map_err(|e| self.error(&e))?;
        let issuer = resource
            .authorization_servers
            .first()
            .ok_or_else(|| self.error("the protected resource names no authorization server"))?;
        let server = discovery::server(&self.web, issuer)
            .await
            .map_err(|e| self.error(&e))?;
        let methods = &server.code_challenge_methods_supported;
        if !methods.is_empty() && !methods.iter().any(|m| m == "S256") {
            return Err(self.error("the authorization server does not support PKCE S256"));
        }
        Ok((resource, server))
    }

    /// The definition's client, else one registered dynamically.
    async fn client(
        &self,
        server: &ServerMetadata,
        resource: &ResourceMetadata,
        redirect: &str,
    ) -> Result<Client, ClientError> {
        let resource =
            discovery::checked_resource(resource, &self.url).map_err(|e| self.error(&e))?;
        let (client_id, client_secret) =
            match (&self.config.client_id, &server.registration_endpoint) {
                (Some(id), _) => (id.clone(), self.config.client_secret.clone()),
                (None, Some(endpoint)) => self.register(endpoint, redirect).await?,
                (None, None) => return Err(ClientError::OAuthClientId(self.server.clone())),
            };
        Ok(Client {
            token_endpoint: server.token_endpoint.clone(),
            client_id,
            client_secret,
            resource,
        })
    }

    /// RFC 7591 dynamic client registration of a public client.
    async fn register(
        &self,
        endpoint: &str,
        redirect: &str,
    ) -> Result<(String, Option<String>), ClientError> {
        #[derive(Deserialize)]
        struct Registered {
            client_id: String,
            client_secret: Option<String>,
        }
        let body = json!({
            "client_name": "rustman",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        });
        let answer = self
            .web
            .post_json(endpoint, body)
            .await
            .map_err(|e| self.error(&format!("client registration: {e}")))?;
        if !(200..300).contains(&answer.status) {
            return Err(self.error(&format!("client registration answered {}", answer.status)));
        }
        let registered: Registered = serde_json::from_slice(&answer.body)
            .map_err(|e| self.error(&format!("client registration: invalid answer: {e}")))?;
        Ok((registered.client_id, registered.client_secret))
    }

    fn scopes(&self, resource: &ResourceMetadata) -> Vec<String> {
        if self.config.scopes.is_empty() {
            resource.scopes_supported.clone()
        } else {
            self.config.scopes.clone()
        }
    }
}

/// The URL the user opens to authorize.
fn authorization_url(
    server: &ServerMetadata,
    client: &Client,
    redirect: &str,
    pkce: &Pkce,
    scopes: &[String],
) -> Result<String, ClientError> {
    let raw = &server.authorization_endpoint;
    let mut url = Url::parse(raw)
        .map_err(|e| ClientError::OAuth(format!("invalid authorization endpoint {raw}: {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", redirect)
            .append_pair("state", &pkce.state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &client.resource);
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url.into())
}

/// The code from the loopback listener, or from a pasted redirect URL.
async fn wait_for_code(
    loopback: &Loopback,
    state: &str,
    pasted: BoxFuture<'static, Option<String>>,
) -> Result<String, String> {
    let listening = loopback.wait_for_code(state, LOGIN_TIMEOUT);
    tokio::select! {
        code = listening => code.map_err(|e| e.to_string()),
        Some(url) = pasted => code_from_redirect(&url, state),
    }
}

/// The `code` of a redirect URL whose `state` is ours.
fn code_from_redirect(redirect: &str, state: &str) -> Result<String, String> {
    let url = Url::parse(redirect.trim()).map_err(|e| format!("invalid redirect URL: {e}"))?;
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if let Some(error) = param("error") {
        return Err(format!("authorization failed: {error}"));
    }
    if param("state").as_deref() != Some(state) {
        return Err("the redirect URL belongs to another login".to_owned());
    }
    param("code").ok_or_else(|| "the redirect URL has no code".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_redirects_are_checked() {
        let url = "http://127.0.0.1:5/callback?code=c0de&state=s1";
        assert_eq!(code_from_redirect(url, "s1").unwrap(), "c0de");
        assert_eq!(
            code_from_redirect(url, "s2").unwrap_err(),
            "the redirect URL belongs to another login"
        );
        assert_eq!(
            code_from_redirect("http://x/cb?error=access_denied&state=s1", "s1").unwrap_err(),
            "authorization failed: access_denied"
        );
        assert!(code_from_redirect("not a url", "s1").is_err());
    }

    #[test]
    fn the_authorization_url_carries_pkce_and_the_resource() {
        let server = ServerMetadata {
            issuer: None,
            authorization_endpoint: "https://as/authorize".into(),
            token_endpoint: "https://as/token".into(),
            registration_endpoint: None,
            code_challenge_methods_supported: vec!["S256".into()],
        };
        let client = Client {
            token_endpoint: "https://as/token".into(),
            client_id: "app".into(),
            client_secret: None,
            resource: "https://h/mcp".into(),
        };
        let pkce = Pkce::new().unwrap();
        let url = authorization_url(
            &server,
            &client,
            "http://127.0.0.1:1/callback",
            &pkce,
            &["read".into()],
        )
        .unwrap();
        let url = Url::parse(&url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["code_challenge"], pkce.challenge);
        assert_eq!(params["resource"], "https://h/mcp");
        assert_eq!(params["scope"], "read");
        assert_eq!(params["client_id"], "app");
    }
}
