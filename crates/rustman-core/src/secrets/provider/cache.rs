//! The in-memory secret cache (design D7): per-key TTL, one fetch shared by
//! concurrent callers, values zeroized when evicted or dropped. Nothing is
//! ever written to disk.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::secret::Secret;

/// The default time a resolved value is reused (`cache_ttl_s`).
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);

struct Cached {
    value: Zeroizing<String>,
    at: Instant,
}

#[derive(Default)]
struct Slot {
    value: async_lock::Mutex<Option<Cached>>,
}

/// Values by key (`<provider>/<path>`), in process memory only.
#[derive(Default)]
pub struct SecretCache {
    slots: Mutex<HashMap<String, Arc<Slot>>>,
}

impl SecretCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `key` cached less than `ttl` ago, or the one `fetch`
    /// produces. Concurrent calls for the same key wait for one fetch. A
    /// zero `ttl` disables caching; errors are never cached.
    pub async fn get_or_fetch<E, F>(
        &self,
        key: &str,
        ttl: Duration,
        fetch: impl FnOnce() -> F,
    ) -> Result<Secret<String>, E>
    where
        F: Future<Output = Result<Secret<String>, E>>,
    {
        let slot = self.slot(key);
        let mut cached = slot.value.lock().await;
        if let Some(entry) = cached.as_ref()
            && entry.at.elapsed() < ttl
        {
            return Ok(Secret::new(key, (*entry.value).clone()));
        }
        *cached = None;
        let value = fetch().await?;
        if !ttl.is_zero() {
            *cached = Some(Cached {
                value: Zeroizing::new(value.expose().clone()),
                at: Instant::now(),
            });
        }
        Ok(value)
    }

    /// Forget every cached value ("Refresh secrets"); they are zeroized.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// How many keys hold a value or a fetch in progress.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn slot(&self, key: &str) -> Arc<Slot> {
        self.lock().entry(key.to_owned()).or_default().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Slot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures_lite::future::block_on;

    use super::*;

    async fn fetcher(count: &AtomicUsize) -> Result<Secret<String>, ()> {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Secret::new("k", format!("v{n}")))
    }

    #[test]
    fn values_are_reused_within_the_ttl() {
        let cache = SecretCache::new();
        let count = AtomicUsize::new(0);
        block_on(async {
            for _ in 0..50 {
                let v = cache
                    .get_or_fetch("vault/kv/app", DEFAULT_TTL, || fetcher(&count))
                    .await
                    .unwrap();
                assert_eq!(v.expose(), "v1");
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        cache.clear();
        assert!(cache.is_empty());
        let v = block_on(cache.get_or_fetch("vault/kv/app", DEFAULT_TTL, || fetcher(&count)));
        assert_eq!(v.unwrap().expose(), "v2");
    }

    #[test]
    fn a_zero_ttl_fetches_every_time_and_errors_are_not_cached() {
        let cache = SecretCache::new();
        let count = AtomicUsize::new(0);
        block_on(async {
            for _ in 0..3 {
                let _ = cache
                    .get_or_fetch("k", Duration::ZERO, || fetcher(&count))
                    .await;
            }
            let failed: Result<_, &str> = cache
                .get_or_fetch("e", DEFAULT_TTL, || async { Err("down") })
                .await;
            assert_eq!(failed.unwrap_err(), "down");
            let ok = cache
                .get_or_fetch("e", DEFAULT_TTL, || fetcher(&count))
                .await;
            assert!(ok.is_ok());
        });
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn concurrent_callers_share_one_fetch() {
        let cache = SecretCache::new();
        let count = AtomicUsize::new(0);
        block_on(async {
            let slow = || async {
                count.fetch_add(1, Ordering::SeqCst);
                futures_timer::Delay::new(Duration::from_millis(50)).await;
                Ok::<_, ()>(Secret::new("k", "v".to_owned()))
            };
            let calls = (0..8).map(|_| cache.get_or_fetch("k", DEFAULT_TTL, slow));
            let results = futures_util::future::join_all(calls).await;
            assert!(results.iter().all(|r| r.as_ref().unwrap().expose() == "v"));
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
