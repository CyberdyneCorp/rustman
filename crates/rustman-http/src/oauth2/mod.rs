//! OAuth2 (`auth: {type: oauth2}`): the `client_credentials` grant and the
//! `authorization_code` grant with PKCE (S256) over a loopback redirect.
//!
//! Tokens are cached per environment in the secret store (design D6), used
//! until shortly before they expire, refreshed with a refresh token when one
//! was issued, and re-acquired when a request gets a `401`. Without the OS
//! keychain they land in `.rustman/secrets.yaml`, which `rustman auth login`
//! reports once (see [`login_warnings`]).

mod loopback;
mod pkce;
mod token;

use std::time::Duration;

use rustman_core::model::{Auth, OAuth2, OAuth2Grant};
use rustman_core::secrets::{FALLBACK_WARNING, StoredIn};
use rustman_core::vars::ResolvedRequest;
use rustman_core::workspace::RequestRef;
use rustman_core::{Redactor, SecretStore};

use crate::cookies::NO_ENVIRONMENT;
use crate::engine::{HttpEngine, SendOptions};
use crate::error::RequestError;
use crate::exchange::Exchange;
pub use loopback::Loopback;
pub use pkce::Pkce;
use token::{TokenCache, TokenEndpoint, TokenSet, now};

/// How long the authorization-code flow waits for the browser.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// The warning `rustman auth login` prints when the token went to the
/// secrets file instead of the OS keychain.
pub const TOKEN_FILE_WARNING: &str = "OAuth2 tokens are stored in .rustman/secrets.yaml \
     (git-ignored, readable by you only), not in the OS keychain";

/// The warnings to print after a login: the secret store's, with the
/// generic keychain-fallback one replaced by [`TOKEN_FILE_WARNING`] when the
/// token was `stored` in the file, so the user is told once.
pub fn login_warnings(store_warnings: Vec<String>, stored: StoredIn) -> Vec<String> {
    let mut warnings = store_warnings;
    if stored == StoredIn::File {
        warnings.retain(|w| w != FALLBACK_WARNING);
        warnings.push(TOKEN_FILE_WARNING.to_owned());
    }
    warnings
}

/// How the authorization-code flow may reach the user.
pub enum Login<'a> {
    /// Show the authorization URL: open a browser and print it.
    Interactive(&'a (dyn Fn(&str) + Send + Sync)),
    /// No terminal to log in from: fail, naming the request to log in with.
    NonInteractive { request: String },
}

/// Where tokens are cached and how to log in.
pub struct OAuth2Context<'a> {
    pub store: &'a SecretStore,
    /// The active environment (`None`: no environment).
    pub env: Option<&'a str>,
    /// The id of the request or collection that defines the auth; see
    /// [`cache_key`].
    pub cache_key: String,
    pub login: Login<'a>,
}

/// The error when a login is needed but there is no terminal.
pub fn interactive_login_required(request: &str) -> RequestError {
    RequestError::Auth(format!(
        "oauth2: interactive login required (run 'rustman auth login {request}')"
    ))
}

/// The id tokens are cached under: the request's id when it defines its own
/// auth, else the folder (`<path>/`, or `./` for the root) whose auth it inherits.
pub fn cache_key(request: &RequestRef<'_>) -> String {
    let own = match request.request() {
        rustman_core::model::Request::Http(http) => http.auth.is_some(),
        rustman_core::model::Request::Graphql(graphql) => graphql.auth.is_some(),
        rustman_core::model::Request::Jsonrpc(jsonrpc) => jsonrpc.auth.is_some(),
        rustman_core::model::Request::Ws(ws) => ws.auth.is_some(),
        rustman_core::model::Request::Sse(sse) => sse.auth.is_some(),
        rustman_core::model::Request::Mcp(_)
        | rustman_core::model::Request::Grpc(_)
        | rustman_core::model::Request::Messaging(_)
        | rustman_core::model::Request::Evm(_)
        | rustman_core::model::Request::Opaque(_) => false,
    };
    let owner = request
        .ancestors
        .iter()
        .rev()
        .find(|c| c.meta.auth.is_some());
    match owner.filter(|_| !own) {
        Some(c) if c.path.as_os_str().is_empty() => "./".to_owned(),
        Some(c) => {
            let parts: Vec<String> = c
                .path
                .components()
                .map(|p| p.as_os_str().to_string_lossy().into_owned())
                .collect();
            format!("{}/", parts.join("/"))
        }
        None => request.entry.id(),
    }
}

impl HttpEngine {
    /// Send `request`, first acquiring an access token when its auth is
    /// `oauth2` (or signing it: `jwt`, `digest`, `aws_sigv4`). A cached token that gets a `401` is replaced and the request
    /// sent once more. Token values are added to `request.secrets`, so every
    /// sink redacts them.
    pub async fn send_authorized(
        &self,
        request: &mut ResolvedRequest,
        options: &SendOptions,
        context: &OAuth2Context<'_>,
    ) -> Result<Exchange, RequestError> {
        if let Some(result) = self.send_signed(request, options, context.env).await {
            return result;
        }
        let Some(Auth::OAuth2(config)) = request.auth.clone() else {
            return self.send(request, options).await;
        };
        let flow = Flow::new(self, &config, request, options, context);
        let (token, acquired) = flow.token().await?;
        token.redact_in(&mut request.secrets);
        let exchange = self
            .send_with_token(request, options, Some(&token.access))
            .await?;
        if exchange.status != 401 || acquired {
            return Ok(exchange);
        }
        let token = flow.reacquire(&token).await?;
        token.redact_in(&mut request.secrets);
        self.send_with_token(request, options, Some(&token.access))
            .await
    }

    /// Acquire a new token for `request` (`rustman auth login`), replacing
    /// any cached one. Returns where the token was stored.
    pub async fn oauth2_login(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        context: &OAuth2Context<'_>,
    ) -> Result<StoredIn, RequestError> {
        let Some(Auth::OAuth2(config)) = &request.auth else {
            return Err(RequestError::Auth(
                "oauth2: the request does not use oauth2 auth".into(),
            ));
        };
        let flow = Flow::new(self, config, request, options, context);
        let token = flow.grant().await?;
        flow.cache.save(&token)
    }
}

/// Getting a token for one request.
struct Flow<'a> {
    endpoint: TokenEndpoint<'a>,
    cache: TokenCache<'a>,
    login: &'a Login<'a>,
    redactor: Redactor,
}

impl<'a> Flow<'a> {
    fn new(
        engine: &'a HttpEngine,
        config: &'a OAuth2,
        request: &ResolvedRequest,
        options: &SendOptions,
        context: &'a OAuth2Context<'a>,
    ) -> Self {
        // A redirect from the token URL would carry the form, with the client
        // secret and codes in it, to wherever `Location` points. The token
        // endpoint (often a third-party identity provider) gets no trace
        // headers: baggage may hold secrets meant for the API only.
        let options = SendOptions {
            cookies: None,
            follow_redirects: false,
            trace: None,
            ..options.clone()
        };
        Self {
            endpoint: TokenEndpoint {
                engine,
                config,
                options,
                secrets: request.secrets.clone(),
            },
            cache: TokenCache {
                store: context.store,
                env: context.env.unwrap_or(NO_ENVIRONMENT),
                name: format!("oauth2:{}", context.cache_key),
            },
            login: &context.login,
            redactor: Redactor::for_request(request),
        }
    }

    /// A usable token, and whether it was just acquired.
    async fn token(&self) -> Result<(TokenSet, bool), RequestError> {
        match self.cache.load()? {
            Some(cached) if cached.is_fresh(now()) => Ok((cached, false)),
            Some(stale) => Ok((self.reacquire(&stale).await?, true)),
            None => Ok((self.acquire().await?, true)),
        }
    }

    /// Replace `stale`: refresh it when possible, else run the grant again.
    async fn reacquire(&self, stale: &TokenSet) -> Result<TokenSet, RequestError> {
        self.cache.remove()?;
        if let Some(refresh) = &stale.refresh
            && let Ok(token) = self.endpoint.refresh(refresh).await
        {
            self.cache.save(&token)?;
            return Ok(token);
        }
        self.acquire().await
    }

    /// Run the grant and cache the token.
    async fn acquire(&self) -> Result<TokenSet, RequestError> {
        let token = self.grant().await?;
        self.cache.save(&token)?;
        Ok(token)
    }

    /// Run the grant.
    async fn grant(&self) -> Result<TokenSet, RequestError> {
        match self.endpoint.config.grant {
            OAuth2Grant::ClientCredentials => self.endpoint.client_credentials().await,
            OAuth2Grant::AuthorizationCode => self.authorize().await,
        }
        .map_err(|e| e.redacted(&self.redactor))
    }

    /// The authorization-code flow: the user approves in a browser, which
    /// brings the code back to the loopback listener.
    async fn authorize(&self) -> Result<TokenSet, RequestError> {
        let show = match self.login {
            Login::Interactive(show) => show,
            Login::NonInteractive { request } => return Err(interactive_login_required(request)),
        };
        let pkce = Pkce::new()?;
        let loopback = Loopback::bind().await?;
        let url = pkce::authorization_url(self.endpoint.config, loopback.redirect_uri(), &pkce)?;
        show(&url);
        let code = loopback.wait_for_code(&pkce.state, LOGIN_TIMEOUT).await?;
        self.endpoint
            .authorization_code(&code, &pkce.verifier, loopback.redirect_uri())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_in_the_file_is_reported_once() {
        let store = vec![FALLBACK_WARNING.to_owned(), "other".to_owned()];
        assert_eq!(
            login_warnings(store.clone(), StoredIn::File),
            ["other", TOKEN_FILE_WARNING]
        );
        assert_eq!(
            login_warnings(Vec::new(), StoredIn::File),
            [TOKEN_FILE_WARNING]
        );
        assert_eq!(login_warnings(store.clone(), StoredIn::Keychain), store);
    }

    #[test]
    fn the_non_interactive_error_names_the_command() {
        assert_eq!(
            interactive_login_required("api/me").to_string(),
            "oauth2: interactive login required (run 'rustman auth login api/me')"
        );
    }
}
