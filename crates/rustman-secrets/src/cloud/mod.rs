//! What the cloud providers share (design D4): credentials that expire,
//! held in memory until shortly before they do, and the short timeout of
//! metadata endpoints that only exist on cloud machines.
//!
//! The providers are thin REST clients on `rustman-http`; each implements
//! its platform's documented credential chain.

#[cfg(feature = "secrets-aws")]
pub mod aws;
#[cfg(feature = "secrets-azure")]
pub mod azure;
#[cfg(feature = "secrets-gcp")]
pub mod gcp;

use std::future::Future;
use std::time::{Duration, Instant};

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::ProviderError;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::rest::Reply;

/// How long a metadata endpoint (EC2, GCE, Azure IMDS) may take: they
/// answer in milliseconds on a cloud machine and not at all elsewhere.
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(1);

/// A credential is renewed when it has less than this left.
const RENEW_BEFORE: Duration = Duration::from_secs(300);

/// How long a credential without a stated lifetime is reused.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(900);

/// A credential and when it expires.
pub struct Expiring<T> {
    pub value: T,
    pub expires: Instant,
}

impl<T> Expiring<T> {
    /// `value`, valid for `lifetime` (a default one when unknown).
    pub fn new(value: T, lifetime: Option<Duration>) -> Self {
        Self {
            value,
            expires: Instant::now() + lifetime.unwrap_or(DEFAULT_LIFETIME),
        }
    }
}

/// One credential, held in memory and renewed shortly before it expires.
pub struct Held<T> {
    current: Mutex<Option<Expiring<T>>>,
}

impl<T> Default for Held<T> {
    fn default() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }
}

impl<T: Clone> Held<T> {
    /// The held credential, or a new one from `obtain`.
    pub async fn get<F, Fut>(&self, obtain: F) -> Result<T, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Expiring<T>, ProviderError>>,
    {
        let mut current = self.current.lock().await;
        if let Some(held) = current.as_ref()
            && held.expires > Instant::now() + RENEW_BEFORE
        {
            return Ok(held.value.clone());
        }
        let fresh = obtain().await?;
        let value = fresh.value.clone();
        *current = Some(fresh);
        Ok(value)
    }
}

/// The lifetime in an OAuth token response: `expires_in` seconds, as a
/// number or (Azure managed identity) a string.
pub fn expires_in(body: &Value) -> Option<Duration> {
    let field = body.get("expires_in")?;
    let seconds = field
        .as_u64()
        .or_else(|| field.as_str().and_then(|s| s.parse().ok()))?;
    Some(Duration::from_secs(seconds))
}

/// The `access_token` of an OAuth token response, as an `Authorization`
/// header value.
pub fn bearer(provider: &str, reply: &Reply) -> Result<Expiring<Secret<String>>, ProviderError> {
    let failed = || {
        ProviderError::Auth(format!(
            "{provider}: authentication failed (token endpoint returned {})",
            reply.status
        ))
    };
    if !(200..300).contains(&reply.status) {
        return Err(failed());
    }
    let body = reply.json().ok_or_else(failed)?;
    let token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(failed)?;
    let header = Secret::new(format!("{provider}_token"), format!("Bearer {token}"));
    Ok(Expiring::new(header, expires_in(&body)))
}

/// Base64-decoded secret bytes as text.
pub fn utf8_text(provider: &str, encoded: &str) -> Result<String, ProviderError> {
    use base64::Engine as _;
    let not_text =
        || ProviderError::Unavailable(format!("{provider}: the secret is not UTF-8 text"));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| not_text())?;
    String::from_utf8(bytes).map_err(|_| not_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credentials_are_reused_until_shortly_before_they_expire() {
        let held = Held::default();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let obtain = |lifetime: u64| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { Ok(Expiring::new(lifetime, Some(Duration::from_secs(lifetime)))) }
        };
        assert_eq!(held.get(|| obtain(3600)).await.unwrap(), 3600);
        assert_eq!(held.get(|| obtain(7)).await.unwrap(), 3600);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let short = Held::default();
        short.get(|| obtain(60)).await.unwrap();
        assert_eq!(short.get(|| obtain(61)).await.unwrap(), 61);
    }

    #[test]
    fn token_responses_and_payloads_are_read() {
        let reply = Reply {
            status: 200,
            body: br#"{"access_token":"tok","expires_in":"3599"}"#.to_vec(),
        };
        let token = bearer("gcp", &reply).unwrap();
        assert_eq!(token.value.expose(), "Bearer tok");
        let reply = Reply {
            status: 401,
            body: br#"{"error":"invalid_client","secret":"tok"}"#.to_vec(),
        };
        let err = bearer("gcp", &reply).err().unwrap().to_string();
        assert_eq!(
            err,
            "gcp: authentication failed (token endpoint returned 401)"
        );
        assert_eq!(utf8_text("x", "aGk=").unwrap(), "hi");
        assert!(utf8_text("x", "/w==").is_err());
    }
}
