//! Secret providers (secret-providers spec): `{{secret:<provider>/<path>}}`
//! references resolved at send time from external stores.
//!
//! This module holds what every provider shares: the reference syntax, the
//! [`SecretProvider`] trait, typed errors, the in-memory cache, the
//! registry of configured providers and their configuration. The provider
//! implementations live in `rustman-secrets`.
//!
//! Resolution happens only at the interpolation boundary of a send
//! ([`resolve_with_secrets`]): a template is resolved once, and when it
//! needs provider values they are fetched together and the template is
//! resolved again with them. Nothing else calls a provider.

mod cache;
pub mod config;
mod error;
pub mod field;
mod reference;
mod registry;

use std::future::Future;
use std::pin::Pin;

pub use cache::{DEFAULT_TTL, SecretCache};
pub use config::{DEFAULT_TIMEOUT, Origin, ProviderConfig};
pub use error::{EXIT_CONFIG, EXIT_REQUEST, ProviderError, UnresolvedSecret};
pub use reference::{
    PREFIX, SecretRef, invalid_reference, malformed_in, mentions_reference, neutralize_references,
};
pub use registry::{MAX_CONCURRENT_FETCHES, ProviderInfo, SecretProviders};

use crate::secret::Secret;
use crate::vars::{ResolveError, Resolver};

/// What a provider fetch returns.
pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Secret<String>, ProviderError>> + Send + 'a>>;

/// What a provider login returns.
pub type LoginFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>>;

/// A source of secret values. Object-safe, so plugins can add providers.
pub trait SecretProvider: Send + Sync {
    /// The whole secret at `path` (field selection is done by the caller).
    /// Errors never contain a value or a credential. Bounded by the
    /// provider's `timeout_ms`.
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a>;

    /// An interactive login (a browser), done before the first fetch and
    /// not bounded by the fetch timeout. Most providers have none.
    fn login<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    /// Check auth and connectivity without reading a secret (`rustman
    /// secret test`), after [`login`](Self::login). Bounded by the
    /// provider's `timeout_ms`. Providers with nothing to check succeed.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

/// Resolve with `resolve`; when the result needs provider values, fetch
/// them through `providers` (none configured: every reference names an
/// unknown provider) and resolve again with them.
pub async fn resolve_with_secrets<T>(
    resolver: &mut Resolver<'_>,
    providers: Option<&SecretProviders>,
    resolve: impl Fn(&Resolver<'_>) -> Result<T, ResolveError>,
) -> Result<T, ResolveError> {
    let references = match resolve(resolver) {
        Err(ResolveError::SecretsNeeded(references)) => references,
        other => return other,
    };
    let none = SecretProviders::new();
    let providers = providers.unwrap_or(&none);
    let values = providers.fetch_all(&references).await?;
    resolver.provide(values);
    resolve(resolver)
}

#[cfg(test)]
mod tests;
