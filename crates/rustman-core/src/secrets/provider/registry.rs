//! The configured providers of a process and the one path that fetches
//! from them (design D1): with a timeout, through the cache, at most
//! [`MAX_CONCURRENT_FETCHES`] at a time for one request.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{Either, select};
use futures_util::stream::{self, StreamExt};

use super::SecretProvider;
use super::cache::SecretCache;
use super::config::{Origin, ProviderConfig};
use super::error::{ProviderError, UnresolvedSecret};
use super::field;
use super::reference::SecretRef;
use crate::secret::Secret;

/// How many references of one request are fetched at once.
pub const MAX_CONCURRENT_FETCHES: usize = 4;

/// What `secret list-providers` shows about a provider; never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    pub name: String,
    pub kind: String,
    pub origin: Origin,
    /// The auth method, for the types that have one (`vault`).
    pub auth: Option<String>,
    pub timeout: Duration,
    pub ttl: Duration,
}

impl ProviderInfo {
    pub fn of(config: &ProviderConfig) -> Self {
        Self {
            name: config.name.clone(),
            kind: config.kind.clone(),
            origin: config.origin,
            auth: auth_method(config),
            timeout: config.timeout,
            ttl: config.ttl,
        }
    }
}

/// The auth method a provider uses: its `auth` setting, else the type's
/// default (`token` for Vault, the CLI or Connect for 1Password, the
/// platform's credential chain for the cloud types).
fn auth_method(config: &ProviderConfig) -> Option<String> {
    if let Some(auth) = config.string("auth") {
        return Some(auth.to_owned());
    }
    let default = match config.kind.as_str() {
        "vault" => "token",
        "onepassword" if config.string("connect_host").is_some() => "connect",
        "onepassword" => "op-cli",
        "aws-secrets-manager" | "aws-ssm" | "gcp-secret-manager" | "azure-key-vault" => {
            "default-chain"
        }
        _ => return None,
    };
    Some(default.to_owned())
}

struct Entry {
    info: ProviderInfo,
    provider: Arc<dyn SecretProvider>,
}

/// Every configured provider, with the process-wide cache.
#[derive(Default)]
pub struct SecretProviders {
    entries: BTreeMap<String, Entry>,
    cache: SecretCache,
}

impl SecretProviders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) the provider described by `info`.
    pub fn insert(&mut self, info: ProviderInfo, provider: Arc<dyn SecretProvider>) {
        self.entries
            .insert(info.name.clone(), Entry { info, provider });
    }

    /// The configured provider names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Every provider's description, sorted by name.
    pub fn infos(&self) -> Vec<&ProviderInfo> {
        self.entries.values().map(|e| &e.info).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget every cached value: the next use fetches again.
    pub fn refresh(&self) {
        self.cache.clear();
    }

    /// The value `reference` names, its field selected.
    pub async fn fetch(&self, reference: &SecretRef) -> Result<Secret<String>, UnresolvedSecret> {
        let unresolved = |error| UnresolvedSecret {
            reference: reference.clone(),
            error,
        };
        let raw = self.raw(reference).await.map_err(unresolved)?;
        let value = match &reference.field {
            Some(name) => field::select(raw.expose(), name).map_err(unresolved)?,
            None => raw.expose().clone(),
        };
        Ok(Secret::new(reference.to_string(), value))
    }

    /// The values of `references`, in order; the first failure in order
    /// stops the request.
    pub async fn fetch_all(
        &self,
        references: &[SecretRef],
    ) -> Result<Vec<(SecretRef, Secret<String>)>, UnresolvedSecret> {
        let fetches = references.iter().map(|r| async move {
            let value = self.fetch(r).await?;
            Ok((r.clone(), value))
        });
        let results: Vec<Result<_, UnresolvedSecret>> = stream::iter(fetches)
            .buffered(MAX_CONCURRENT_FETCHES)
            .collect()
            .await;
        results.into_iter().collect()
    }

    /// Check that provider `name` can authenticate and be reached
    /// (`rustman secret test`), within its timeout. Reads no secret.
    pub async fn check(&self, name: &str) -> Result<(), ProviderError> {
        let entry = self.entry(name)?;
        entry.provider.login().await?;
        within(entry.provider.check(), &entry.info.name, entry.info.timeout).await
    }

    fn entry(&self, name: &str) -> Result<&Entry, ProviderError> {
        self.entries
            .get(name)
            .ok_or_else(|| ProviderError::UnknownProvider {
                name: name.to_owned(),
                configured: self.names(),
            })
    }

    async fn raw(&self, reference: &SecretRef) -> Result<Secret<String>, ProviderError> {
        let entry = self.entry(&reference.provider)?;
        let info = &entry.info;
        let fetch = || async {
            entry.provider.login().await?;
            let fetched = entry.provider.fetch(&reference.path);
            within(fetched, &info.name, info.timeout).await
        };
        let key = reference.location();
        self.cache.get_or_fetch(&key, info.ttl, fetch).await
    }
}

/// `fetch`, or a timeout error naming `provider` once `limit` elapses.
async fn within<T, F>(fetch: F, provider: &str, limit: Duration) -> Result<T, ProviderError>
where
    F: Future<Output = Result<T, ProviderError>>,
{
    let delay = futures_timer::Delay::new(limit);
    match select(pin!(fetch), delay).await {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => Err(ProviderError::Timeout {
            provider: provider.to_owned(),
            ms: u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::secrets::provider::config::merge;

    fn auth_of(table: &str) -> Option<String> {
        let table: toml::Table = toml::from_str(table).unwrap();
        let workspace = BTreeMap::from([("p".to_owned(), table)]);
        let config = merge(&workspace, &BTreeMap::new()).unwrap().remove(0);
        ProviderInfo::of(&config).auth
    }

    #[test]
    fn every_type_with_an_auth_method_lists_it() {
        assert_eq!(auth_of("type = \"vault\"").as_deref(), Some("token"));
        assert_eq!(
            auth_of("type = \"vault\"\nauth = \"oidc\"").as_deref(),
            Some("oidc")
        );
        assert_eq!(auth_of("type = \"onepassword\"").as_deref(), Some("op-cli"));
        assert_eq!(
            auth_of("type = \"onepassword\"\nconnect_host = \"https://c\"").as_deref(),
            Some("connect")
        );
        assert_eq!(
            auth_of("type = \"aws-ssm\"").as_deref(),
            Some("default-chain")
        );
        assert_eq!(auth_of("type = \"env\""), None);
    }
}
