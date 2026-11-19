//! Tokens of an OAuth-protected MCP server: kept as one secret in the
//! [`SecretStore`] (the OS keychain, else the git-ignored
//! `.rustman/secrets.yaml`) under `mcp/<server-hash>`, with what refreshing
//! them needs, so a refresh never opens the browser.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ring::digest::{SHA256, digest};
use rustman_core::{Secret, SecretStore};
use rustman_http::NO_ENVIRONMENT;
use serde::{Deserialize, Serialize};

use super::web::Web;

/// A token this close to its expiry is refreshed first.
const EXPIRY_MARGIN_SECS: u64 = 30;

/// The names token values are redacted as.
pub(crate) const ACCESS_TOKEN: &str = "mcp_oauth_access_token";
const REFRESH_TOKEN: &str = "mcp_oauth_refresh_token";
const CLIENT_SECRET: &str = "mcp_oauth_client_secret";

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The client a token was issued to, and where to refresh it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Client {
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// The `resource` parameter (RFC 8707): the MCP server.
    pub resource: String,
}

/// Tokens and their client, as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Grant {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub client: Client,
}

impl Grant {
    pub fn is_fresh(&self, now: u64) -> bool {
        self.expires_at
            .is_none_or(|at| at > now.saturating_add(EXPIRY_MARGIN_SECS))
    }

    pub fn access(&self) -> Secret<String> {
        Secret::new(ACCESS_TOKEN, self.access_token.clone())
    }

    /// Every value of the grant to redact.
    pub fn secrets(&self) -> Vec<Secret<String>> {
        let mut secrets = vec![self.access()];
        secrets.extend(
            self.refresh_token
                .clone()
                .map(|r| Secret::new(REFRESH_TOKEN, r)),
        );
        secrets.extend(self.client.secrets());
        secrets
    }
}

impl Client {
    pub fn secrets(&self) -> Vec<Secret<String>> {
        self.client_secret
            .clone()
            .map(|s| Secret::new(CLIENT_SECRET, s))
            .into_iter()
            .collect()
    }
}

/// The secret name of the tokens of server `name` at `url`.
pub fn store_key(name: &str, url: &str) -> String {
    let hash = digest(&SHA256, format!("{name}\n{url}").as_bytes());
    let hex: String = hash.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("mcp/{hex}")
}

/// One server's entry in the secret store.
#[derive(Clone)]
pub(crate) struct TokenStore {
    pub store: Arc<SecretStore>,
    pub key: String,
}

impl TokenStore {
    pub fn load(&self) -> Result<Option<Grant>, String> {
        let secret = self
            .store
            .get(NO_ENVIRONMENT, &self.key)
            .map_err(|e| format!("token store: {e}"))?;
        Ok(secret.and_then(|s| serde_json::from_str(s.expose()).ok()))
    }

    pub fn save(&self, grant: &Grant) -> Result<(), String> {
        let text = serde_json::to_string(grant).expect("a grant serializes");
        self.store
            .set(NO_ENVIRONMENT, &Secret::new(self.key.clone(), text))
            .map(|_| ())
            .map_err(|e| format!("token store: {e}"))
    }

    pub fn remove(&self) -> Result<bool, String> {
        self.store
            .remove(NO_ENVIRONMENT, &self.key)
            .map_err(|e| format!("token store: {e}"))
    }
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
}

/// POST `params` (plus the client and `resource`) to the token endpoint.
pub(crate) async fn request_token(
    web: &Web,
    client: &Client,
    mut params: Vec<(&str, String)>,
) -> Result<Grant, String> {
    params.push(("client_id", client.client_id.clone()));
    if let Some(secret) = &client.client_secret {
        params.push(("client_secret", secret.clone()));
    }
    params.push(("resource", client.resource.clone()));
    let secrets = params
        .iter()
        .filter(|(n, _)| {
            matches!(
                *n,
                "code" | "code_verifier" | "refresh_token" | "client_secret"
            )
        })
        .map(|(n, v)| Secret::new(format!("oauth_{n}"), v.clone()))
        .collect();
    let answer = web
        .post_form(&client.token_endpoint, &params, secrets)
        .await?;
    if !(200..300).contains(&answer.status) {
        let detail = serde_json::from_slice::<ErrorResponse>(&answer.body)
            .map(|e| format!(" ({})", e.error))
            .unwrap_or_default();
        return Err(format!("token endpoint answered {}{detail}", answer.status));
    }
    let token: TokenResponse = serde_json::from_slice(&answer.body)
        .map_err(|_| "invalid token response (expected JSON with access_token)".to_owned())?;
    Ok(Grant {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: token.expires_in.map(|s| now().saturating_add(s)),
        client: client.clone(),
    })
}

/// A new grant from `grant`'s refresh token; the old refresh token is kept
/// when the server does not rotate it.
pub(crate) async fn refresh(web: &Web, grant: &Grant) -> Result<Grant, String> {
    let refresh = grant
        .refresh_token
        .clone()
        .ok_or_else(|| "no refresh token".to_owned())?;
    let params = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh.clone()),
    ];
    let mut renewed = request_token(web, &grant.client, params).await?;
    renewed.refresh_token.get_or_insert(refresh);
    Ok(renewed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(expires_at: Option<u64>) -> Grant {
        Grant {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at,
            client: Client {
                token_endpoint: "http://as/token".into(),
                client_id: "app".into(),
                client_secret: Some("cs".into()),
                resource: "http://h/mcp".into(),
            },
        }
    }

    #[test]
    fn freshness_keeps_a_margin() {
        assert!(grant(None).is_fresh(100));
        assert!(grant(Some(200)).is_fresh(100));
        assert!(!grant(Some(120)).is_fresh(100));
    }

    #[test]
    fn every_value_is_redacted() {
        let names: Vec<String> = grant(None)
            .secrets()
            .iter()
            .map(|s| s.expose().clone())
            .collect();
        assert_eq!(names, ["a", "r", "cs"]);
    }

    #[test]
    fn keys_depend_on_name_and_url() {
        let a = store_key("files", "http://h/mcp");
        assert!(a.starts_with("mcp/") && a.len() == 20, "{a}");
        assert_ne!(a, store_key("files", "http://other/mcp"));
        assert_eq!(a, store_key("files", "http://h/mcp"));
    }

    #[test]
    fn grants_round_trip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore {
            store: Arc::new(SecretStore::with_keychain(dir.path(), None)),
            key: store_key("files", "http://h/mcp"),
        };
        assert_eq!(store.load().unwrap(), None);
        store.save(&grant(Some(5))).unwrap();
        assert_eq!(store.load().unwrap(), Some(grant(Some(5))));
        assert!(store.remove().unwrap());
        assert_eq!(store.load().unwrap(), None);
    }
}
