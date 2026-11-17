//! PKCE (RFC 7636) with the `S256` method, and the authorization URL.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use rustman_core::model::OAuth2;

use crate::error::RequestError;

/// A fresh code verifier, its `S256` challenge and an anti-CSRF `state`.
pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl Pkce {
    pub fn new() -> Result<Self, RequestError> {
        let verifier = random_token(32)?;
        Ok(Self {
            challenge: challenge_for(&verifier),
            verifier,
            state: random_token(16)?,
        })
    }
}

/// `BASE64URL(SHA256(verifier))` without padding.
pub(crate) fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

/// `len` random bytes, base64url-encoded (43 characters for 32 bytes).
fn random_token(len: usize) -> Result<String, RequestError> {
    let mut bytes = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RequestError::Auth("oauth2: no secure random source".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The URL the user opens to authorize: `auth_url` plus the code-flow
/// parameters.
pub(crate) fn authorization_url(
    config: &OAuth2,
    redirect_uri: &str,
    pkce: &Pkce,
) -> Result<String, RequestError> {
    let raw = config.auth_url.as_deref().unwrap_or_default();
    let mut url = url::Url::parse(raw)
        .map_err(|e| RequestError::Auth(format!("oauth2: invalid auth_url '{raw}': {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", &pkce.state)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(scopes) = config.scopes.as_ref().filter(|s| !s.is_empty()) {
            query.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::model::OAuth2Grant;

    #[test]
    fn the_challenge_is_base64url_sha256() {
        // Computed independently with Python's hashlib and base64.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mJ92K1fV0oQxIkAZ8hB9tL0R5y7zBc"),
            "JM3ziG-Okmc16g1-jZK1GyH-CYE5yZAGrtUArL84VRM"
        );
    }

    #[test]
    fn verifiers_are_long_random_and_url_safe() {
        let a = Pkce::new().unwrap();
        let b = Pkce::new().unwrap();
        assert_eq!(a.verifier.len(), 43);
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
        assert!(
            a.verifier
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_eq!(a.challenge, challenge_for(&a.verifier));
    }

    #[test]
    fn the_url_carries_every_parameter() {
        let config = OAuth2 {
            grant: OAuth2Grant::AuthorizationCode,
            token_url: "https://id.test/token".into(),
            auth_url: Some("https://id.test/authorize?prompt=login".into()),
            client_id: "app".into(),
            client_secret: None,
            scopes: Some(vec!["read".into(), "write".into()]),
        };
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "ch".into(),
            state: "st".into(),
        };
        let url = authorization_url(&config, "http://127.0.0.1:5/callback", &pkce).unwrap();
        assert_eq!(
            url,
            "https://id.test/authorize?prompt=login&response_type=code&client_id=app\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A5%2Fcallback&state=st&code_challenge=ch\
             &code_challenge_method=S256&scope=read+write"
        );
        let bad = OAuth2 {
            auth_url: Some("not a url".into()),
            ..config
        };
        let err = authorization_url(&bad, "x", &pkce).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("oauth2: invalid auth_url 'not a url'"),
            "{err}"
        );
    }
}
