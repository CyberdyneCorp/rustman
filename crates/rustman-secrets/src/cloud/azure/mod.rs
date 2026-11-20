//! `azure-key-vault` (cargo feature `secrets-azure`): `<name>[/version]`
//! from the Key Vault named by `vault`, over its REST API, with a token
//! from `DefaultAzureCredential`'s chain.

mod credential;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use serde_json::Value;

pub use credential::{Credential, KIND};

use crate::cloud::Held;
use crate::context::Context;
use crate::rest::{Call, Reply, segment};

const API_VERSION: &str = "7.4";

pub struct Azure {
    /// `https://<vault>.vault.azure.net`, or the user config's endpoint.
    url: Result<String, ProviderError>,
    credential: Credential,
    token: Held<Secret<String>>,
}

impl Azure {
    /// `endpoint` comes from the user config only.
    pub fn new(
        vault: Option<&str>,
        endpoint: Option<&str>,
        ctx: Context,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            url: vault_url(vault, endpoint),
            credential: Credential::new(ctx, timeout),
            token: Held::default(),
        }
    }

    async fn token(&self) -> Result<Secret<String>, ProviderError> {
        self.token.get(|| self.credential.token()).await
    }

    async fn read(&self, path: &str) -> Result<String, ProviderError> {
        let base = self.url.clone()?;
        let (name, version) = match path.split_once('/') {
            Some((name, version)) => (name, Some(version)),
            None => (path, None),
        };
        if name.is_empty() || version.is_some_and(|v| v.is_empty() || v.contains('/')) {
            return Err(ProviderError::Config(format!(
                "{KIND}: path '{path}' must be <name>[/version]"
            )));
        }
        let version = version.map_or(String::new(), |v| format!("/{}", segment(v)));
        let url = format!(
            "{base}/secrets/{}{version}?api-version={API_VERSION}",
            segment(name)
        );
        let token = self.token().await?;
        let reply = self
            .credential
            .rest()
            .send(Call::get(url).credential("Authorization", &token))
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{KIND}: {e}")))?;
        let body = check(&reply, path)?;
        body.get("value")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ProviderError::Unavailable(format!("{KIND}: invalid response")))
    }
}

/// The vault's URL: a vault name is 3 to 24 letters, digits and dashes.
fn vault_url(vault: Option<&str>, endpoint: Option<&str>) -> Result<String, ProviderError> {
    if let Some(endpoint) = endpoint {
        return Ok(endpoint.trim_end_matches('/').to_owned());
    }
    let vault =
        vault.ok_or_else(|| ProviderError::Config(format!("{KIND}: no vault (set 'vault')")))?;
    let valid = (3..=24).contains(&vault.len())
        && vault.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(ProviderError::Config(format!(
            "{KIND}: invalid vault name '{vault}'"
        )));
    }
    Ok(format!("https://{vault}.vault.azure.net"))
}

fn check(reply: &Reply, path: &str) -> Result<Value, ProviderError> {
    match reply.status {
        200..=299 => reply
            .json()
            .ok_or_else(|| ProviderError::Unavailable(format!("{KIND}: invalid response"))),
        401 => Err(ProviderError::Auth(format!(
            "{KIND}: authentication failed"
        ))),
        403 => Err(ProviderError::Auth(format!(
            "{KIND}: permission denied for {path}"
        ))),
        404 => Err(ProviderError::NotFound(format!(
            "{KIND}: secret {path} not found"
        ))),
        status => Err(ProviderError::Unavailable(format!(
            "{KIND}: server returned {status} for {path}"
        ))),
    }
}

impl SecretProvider for Azure {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Ok(Secret::new(path, self.read(path).await?)) })
    }

    /// Obtains a token.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move {
            self.url.clone()?;
            self.token().await.map(|_| ())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_names_cannot_change_the_host() {
        assert_eq!(
            vault_url(Some("my-vault"), None).unwrap(),
            "https://my-vault.vault.azure.net"
        );
        for bad in ["evil.com/x", "a", "x@y", "a.b"] {
            assert!(vault_url(Some(bad), None).is_err(), "{bad}");
        }
        assert!(vault_url(None, None).is_err());
        assert_eq!(
            vault_url(Some("x"), Some("http://127.0.0.1:1/")).unwrap(),
            "http://127.0.0.1:1"
        );
    }
}
