//! Access tokens: the cache in the secret store and the token endpoint.

use std::time::{SystemTime, UNIX_EPOCH};

use rustman_core::model::{OAuth2, RequestSettings};
use rustman_core::vars::UsedSecret;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_core::{Redactor, Secret, SecretStore};
use serde::{Deserialize, Serialize};

use crate::engine::{HttpEngine, SendOptions};
use crate::error::RequestError;

/// A token this close to its expiry is treated as expired.
const EXPIRY_MARGIN_SECS: u64 = 30;

/// The name the access token is redacted as.
pub(crate) const ACCESS_TOKEN: &str = "oauth2_access_token";
const REFRESH_TOKEN: &str = "oauth2_refresh_token";

/// Seconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An access token, its refresh token and when it expires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenSet {
    pub access: Secret<String>,
    pub refresh: Option<Secret<String>>,
    /// Unix seconds; `None` when the server gave no lifetime.
    pub expires_at: Option<u64>,
}

/// The cached form, kept as JSON in one secret.
#[derive(Serialize, Deserialize)]
struct Stored {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
}

impl TokenSet {
    /// Usable at `now`, with a safety margin before the expiry.
    pub fn is_fresh(&self, now: u64) -> bool {
        self.expires_at
            .is_none_or(|at| at > now.saturating_add(EXPIRY_MARGIN_SECS))
    }

    /// Add the token values to `secrets`, so every sink redacts them.
    pub fn redact_in(&self, secrets: &mut Vec<UsedSecret>) {
        secrets.push(self.access.clone());
        secrets.extend(self.refresh.clone());
    }

    fn to_secret(&self, name: &str) -> Secret<String> {
        let stored = Stored {
            access_token: self.access.expose().clone(),
            refresh_token: self.refresh.as_ref().map(|r| r.expose().clone()),
            expires_at: self.expires_at,
        };
        Secret::new(
            name,
            serde_json::to_string(&stored).expect("tokens serialize"),
        )
    }

    fn from_secret(secret: &Secret<String>) -> Option<Self> {
        let stored: Stored = serde_json::from_str(secret.expose()).ok()?;
        Some(Self {
            access: Secret::new(ACCESS_TOKEN, stored.access_token),
            refresh: stored.refresh_token.map(|r| Secret::new(REFRESH_TOKEN, r)),
            expires_at: stored.expires_at,
        })
    }
}

/// The cached token of one request or collection in one environment,
/// stored as secret `oauth2:<id>` (account `<workspace-id>/<env>/oauth2:<id>`).
pub(crate) struct TokenCache<'a> {
    pub store: &'a SecretStore,
    pub env: &'a str,
    pub name: String,
}

impl TokenCache<'_> {
    /// The cached token; an unreadable entry counts as none.
    pub fn load(&self) -> Result<Option<TokenSet>, RequestError> {
        let secret = self.store.get(self.env, &self.name).map_err(cache_error)?;
        Ok(secret.as_ref().and_then(TokenSet::from_secret))
    }

    pub fn save(&self, token: &TokenSet) -> Result<(), RequestError> {
        self.store
            .set(self.env, &token.to_secret(&self.name))
            .map(|_| ())
            .map_err(cache_error)
    }

    pub fn remove(&self) -> Result<(), RequestError> {
        self.store
            .remove(self.env, &self.name)
            .map(|_| ())
            .map_err(cache_error)
    }
}

fn cache_error(err: rustman_core::SecretError) -> RequestError {
    RequestError::Auth(format!("oauth2: token cache: {err}"))
}

/// The token endpoint of one OAuth2 configuration.
pub(crate) struct TokenEndpoint<'a> {
    pub engine: &'a HttpEngine,
    pub config: &'a OAuth2,
    /// The transport settings of the request being authorized, without cookies.
    pub options: SendOptions,
    /// Secrets of the request being authorized (a client secret, say).
    pub secrets: Vec<UsedSecret>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

impl TokenEndpoint<'_> {
    pub async fn client_credentials(&self) -> Result<TokenSet, RequestError> {
        self.call(vec![("grant_type", "client_credentials".into())], true)
            .await
    }

    pub async fn authorization_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenSet, RequestError> {
        let params = vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.into()),
            ("redirect_uri", redirect_uri.into()),
            ("code_verifier", verifier.into()),
        ];
        self.call(params, false).await
    }

    /// A new token from `refresh`; the old refresh token is kept when the
    /// server does not rotate it.
    pub async fn refresh(&self, refresh: &Secret<String>) -> Result<TokenSet, RequestError> {
        let params = vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh.expose().clone()),
        ];
        let mut token = self.call(params, false).await?;
        token.refresh.get_or_insert_with(|| refresh.clone());
        Ok(token)
    }

    /// POST `params` (plus the client credentials and, when `scoped`, the
    /// scopes) to the token URL.
    async fn call(
        &self,
        mut params: Vec<(&str, String)>,
        scoped: bool,
    ) -> Result<TokenSet, RequestError> {
        params.push(("client_id", self.config.client_id.clone()));
        if let Some(secret) = &self.config.client_secret {
            params.push(("client_secret", secret.clone()));
        }
        if let Some(scopes) = self
            .config
            .scopes
            .as_ref()
            .filter(|s| scoped && !s.is_empty())
        {
            params.push(("scope", scopes.join(" ")));
        }
        let request = self.request(&params);
        let redactor = Redactor::for_request(&request);
        let exchange = self
            .engine
            .send(&request, &self.options)
            .await
            .map_err(|e| RequestError::Auth(format!("oauth2: token endpoint: {e}")))?;
        parse(exchange.status, &exchange.body, now()).map_err(|e| e.redacted(&redactor))
    }

    fn request(&self, params: &[(&str, String)]) -> ResolvedRequest {
        let mut secrets = self.secrets.clone();
        for (name, value) in params {
            if matches!(
                *name,
                "client_secret" | "code" | "code_verifier" | "refresh_token"
            ) {
                secrets.push(Secret::new(format!("oauth2_{name}"), value.clone()));
            }
        }
        let pair = |name: &str, value: &str| ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        };
        ResolvedRequest {
            method: "POST".into(),
            url: Resolved::literal(self.config.token_url.clone()),
            query: Vec::new(),
            headers: vec![pair("Accept", "application/json")],
            body: ResolvedBody::Form(params.iter().map(|(n, v)| pair(n, v)).collect()),
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets,
        }
    }
}

/// The token in a token-endpoint response received at `now`.
fn parse(status: u16, body: &[u8], now: u64) -> Result<TokenSet, RequestError> {
    if !(200..300).contains(&status) {
        let detail = serde_json::from_slice::<ErrorResponse>(body)
            .map(|e| match e.error_description {
                Some(d) => format!(" ({}: {d})", e.error),
                None => format!(" ({})", e.error),
            })
            .unwrap_or_default();
        return Err(RequestError::Auth(format!(
            "oauth2: token endpoint returned {status}{detail}"
        )));
    }
    let response: TokenResponse = serde_json::from_slice(body).map_err(|_| {
        RequestError::Auth(
            "oauth2: invalid token response (expected JSON with access_token)".into(),
        )
    })?;
    Ok(TokenSet {
        access: Secret::new(ACCESS_TOKEN, response.access_token),
        refresh: response
            .refresh_token
            .map(|r| Secret::new(REFRESH_TOKEN, r)),
        expires_at: response.expires_in.map(|s| now.saturating_add(s)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_at: Option<u64>) -> TokenSet {
        TokenSet {
            access: Secret::new(ACCESS_TOKEN, "a".to_owned()),
            refresh: Some(Secret::new(REFRESH_TOKEN, "r".to_owned())),
            expires_at,
        }
    }

    #[test]
    fn tokens_are_fresh_until_shortly_before_expiry() {
        // Issued at 1000 with expires_in 3600: still fresh 10 s later.
        let t = token(Some(4_600));
        assert!(t.is_fresh(1_010));
        assert!(t.is_fresh(4_569));
        assert!(!t.is_fresh(4_570));
        assert!(token(None).is_fresh(u64::MAX));
    }

    #[test]
    fn responses_are_parsed() {
        let body = br#"{"access_token":"at","token_type":"Bearer","expires_in":3600,"refresh_token":"rt"}"#;
        let t = parse(200, body, 1_000).unwrap();
        assert_eq!(t.access.expose(), "at");
        assert_eq!(t.refresh.unwrap().expose(), "rt");
        assert_eq!(t.expires_at, Some(4_600));
        let t = parse(200, br#"{"access_token":"at"}"#, 1).unwrap();
        assert_eq!((t.refresh, t.expires_at), (None, None));
    }

    #[test]
    fn errors_name_the_status_and_oauth_error() {
        let err = parse(
            400,
            br#"{"error":"invalid_client","error_description":"bad secret"}"#,
            0,
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "oauth2: token endpoint returned 400 (invalid_client: bad secret)"
        );
        let err = parse(401, br#"{"error":"invalid_grant"}"#, 0).unwrap_err();
        assert_eq!(
            err.to_string(),
            "oauth2: token endpoint returned 401 (invalid_grant)"
        );
        let err = parse(500, b"oops", 0).unwrap_err();
        assert_eq!(err.to_string(), "oauth2: token endpoint returned 500");
        let err = parse(200, b"{\"token\":\"x\"}", 0).unwrap_err();
        assert_eq!(
            err.to_string(),
            "oauth2: invalid token response (expected JSON with access_token)"
        );
    }

    #[test]
    fn tokens_round_trip_through_the_store_without_leaking() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::with_keychain(dir.path(), None);
        let cache = TokenCache {
            store: &store,
            env: "dev",
            name: "oauth2:api/me".into(),
        };
        assert_eq!(cache.load().unwrap(), None);
        cache.save(&token(Some(42))).unwrap();
        assert_eq!(cache.load().unwrap(), Some(token(Some(42))));
        assert!(!format!("{:?}", token(Some(42))).contains("\"a\""));
        cache.remove().unwrap();
        assert_eq!(cache.load().unwrap(), None);
    }

    #[test]
    fn token_values_join_the_redacted_secrets() {
        let mut secrets = Vec::new();
        token(None).redact_in(&mut secrets);
        let names: Vec<&str> = secrets.iter().map(|s| s.name()).collect();
        assert_eq!(names, [ACCESS_TOKEN, REFRESH_TOKEN]);
    }
}
