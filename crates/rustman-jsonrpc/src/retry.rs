//! Retries for idempotent calls (design D6): JSON-RPC is POST-only, so
//! only calls marked `idempotent: true` are ever sent again. Triggers are
//! connection errors, timeouts, HTTP 429/502/503/504 and the error code
//! `-32005` (limit exceeded); the wait doubles each time from
//! `backoff_ms`, and a server's `Retry-After` replaces it.

use std::time::Duration;

use rustman_core::vars::ResolvedJsonrpc;
use tokio_util::sync::CancellationToken;

use crate::outcome::{CallOutcome, CallResult};

/// The JSON-RPC error code of a rate limit ("limit exceeded").
pub const LIMIT_EXCEEDED: i64 = -32005;

/// The longest `Retry-After` honored; a longer one is cut to this.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// How often and how patiently a request is sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts after the first.
    pub retries: u32,
    /// The wait before the first retry.
    pub backoff: Duration,
}

impl RetryPolicy {
    /// No retries.
    pub const NONE: Self = Self {
        retries: 0,
        backoff: Duration::ZERO,
    };

    /// The policy of `target`: its `settings.retry`, but none unless it is
    /// marked idempotent.
    pub fn of(target: &ResolvedJsonrpc) -> Self {
        if !target.idempotent {
            return Self::NONE;
        }
        Self {
            retries: target.retry.effective_attempts(),
            backoff: Duration::from_millis(target.retry.effective_backoff_ms()),
        }
    }

    /// The wait before retry `n` (1-based): the backoff doubled each time,
    /// or the server's `Retry-After`.
    pub fn delay(&self, n: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(after) = retry_after {
            return after.min(MAX_RETRY_AFTER);
        }
        let factor = 1u32.checked_shl(n.saturating_sub(1)).unwrap_or(u32::MAX);
        self.backoff.saturating_mul(factor)
    }
}

/// HTTP statuses that mean "try again later".
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

/// Whether some call failed with [`LIMIT_EXCEEDED`].
pub fn limit_exceeded(calls: &[CallResult]) -> bool {
    calls
        .iter()
        .any(|c| matches!(&c.outcome, CallOutcome::Error(e) if e.code == LIMIT_EXCEEDED))
}

/// A `Retry-After` value: delay seconds or an HTTP date (RFC 9110).
pub use rustman_core::safety::parse_retry_after as retry_after;

/// Wait `delay`; `false` when `cancel` fired first.
pub(crate) async fn pause(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    #[test]
    fn backoff_doubles_and_retry_after_wins() {
        let policy = RetryPolicy {
            retries: 3,
            backoff: Duration::from_millis(500),
        };
        assert_eq!(policy.delay(1, None), Duration::from_millis(500));
        assert_eq!(policy.delay(2, None), Duration::from_millis(1000));
        assert_eq!(policy.delay(3, None), Duration::from_millis(2000));
        let after = Some(Duration::from_secs(2));
        assert_eq!(policy.delay(1, after), Duration::from_secs(2));
        let long = Some(Duration::from_secs(3600));
        assert_eq!(policy.delay(1, long), MAX_RETRY_AFTER);
        assert!(policy.delay(64, None) > Duration::ZERO, "no overflow");
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert_eq!(retry_after(" 3 ", now), Some(Duration::from_secs(3)));
        let later = httpdate::fmt_http_date(now + Duration::from_secs(5));
        assert_eq!(retry_after(&later, now), Some(Duration::from_secs(5)));
        let earlier = httpdate::fmt_http_date(now - Duration::from_secs(5));
        assert_eq!(retry_after(&earlier, now), Some(Duration::ZERO));
        assert_eq!(retry_after("soon", now), None);
    }

    #[test]
    fn retryable_statuses() {
        for status in [429, 502, 503, 504] {
            assert!(is_retryable_status(status));
        }
        for status in [200, 400, 500] {
            assert!(!is_retryable_status(status));
        }
    }
}
