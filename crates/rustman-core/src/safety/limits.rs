//! Rate, concurrency and budgets for governed senders (design D4 and D5).
//!
//! A [`Governor`] is shared by every request of one session. Each probe is
//! admitted once ([`Governor::admit`]: probe budget, time budget and a
//! concurrency slot) and every request it sends, redirect hops included, is
//! paced ([`Governor::pace`]: `Retry-After` pauses and the rate limit). A
//! `429 Too Many Requests` reported through [`Governor::observe`] halves the
//! rate and pauses sending for its `Retry-After`.
//!
//! Nothing here does I/O: the transport that sends the requests lives in
//! `rustman-http`, and other senders (load tests, agents) can reuse this.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use async_lock::{Semaphore, SemaphoreGuardArc};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

use super::config::SafetySettings;

/// Default probe budget of an exploration.
pub const DEFAULT_MAX_PROBES: u64 = 2000;
/// Default time budget of an exploration.
pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(15 * 60);

/// The status that slows a governed sender down.
pub const TOO_MANY_REQUESTS: u16 = 429;

/// The limits one [`Governor`] enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Requests per second at the start (`--rate`).
    pub rate: NonZeroU32,
    /// Probes in flight at once (`--concurrency`); a probe holds one slot
    /// across its redirect hops.
    pub concurrency: NonZeroU32,
    /// Probes admitted at most (`--max-probes`).
    pub max_probes: u64,
    /// Time after which nothing more is sent (`--max-duration`), counted from
    /// [`Governor::new`]: create the governor when execution starts, after
    /// discovery and planning.
    pub max_duration: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self::from_settings(&SafetySettings::default())
    }
}

impl Limits {
    /// `[safety]` rate and concurrency with the default budgets.
    pub fn from_settings(settings: &SafetySettings) -> Self {
        Self {
            rate: settings.rate,
            concurrency: settings.concurrency,
            max_probes: DEFAULT_MAX_PROBES,
            max_duration: DEFAULT_MAX_DURATION,
        }
    }
}

/// Why a probe was not sent: a budget ran out. Reports show it as
/// `not run: budget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BudgetExhausted {
    #[error("not run: budget")]
    Probes,
    #[error("not run: budget")]
    Duration,
}

impl BudgetExhausted {
    /// Which budget ran out, for logs and progress output.
    pub fn detail(self) -> &'static str {
        match self {
            Self::Probes => "probe budget reached",
            Self::Duration => "time budget reached",
        }
    }
}

/// A probe's concurrency slot; released when dropped.
#[derive(Debug)]
pub struct Admission {
    _slot: SemaphoreGuardArc,
}

/// Proof that a request was paced, naming the rate it was sent at. Pass it
/// back to [`Governor::observe`] with the response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paced {
    generation: u64,
}

/// What [`Governor::observe`] did about a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// The rate from now on.
    pub rate: NonZeroU32,
    /// How long nothing is sent, from `Retry-After`.
    pub pause: Option<Duration>,
}

struct Pace {
    rate: NonZeroU32,
    limiter: Arc<DefaultDirectRateLimiter>,
    /// Bumped on every halving, so the 429s of requests sent at the old rate
    /// halve it only once.
    generation: u64,
    resume_at: Option<Instant>,
    /// Set by a `Retry-After` that ends after the time budget: nothing more
    /// is sent.
    halted: bool,
}

/// Enforces [`Limits`] across concurrent requests.
pub struct Governor {
    limits: Limits,
    started: Instant,
    admitted: AtomicU64,
    slots: Arc<Semaphore>,
    pace: Mutex<Pace>,
}

impl std::fmt::Debug for Governor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Governor")
            .field("limits", &self.limits)
            .field("rate", &self.rate())
            .field("admitted", &self.admitted())
            .finish_non_exhaustive()
    }
}

impl Governor {
    /// Start the clock of the time budget now, so create the governor when
    /// execution starts.
    pub fn new(limits: Limits) -> Self {
        let slots = usize::try_from(limits.concurrency.get()).unwrap_or(usize::MAX);
        Self {
            limits,
            started: Instant::now(),
            admitted: AtomicU64::new(0),
            slots: Arc::new(Semaphore::new(slots)),
            pace: Mutex::new(Pace {
                rate: limits.rate,
                limiter: Arc::new(limiter(limits.rate)),
                generation: 0,
                resume_at: None,
                halted: false,
            }),
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The current rate: the configured one, halved on every backoff.
    pub fn rate(&self) -> NonZeroU32 {
        self.state().rate
    }

    /// Probes admitted so far.
    pub fn admitted(&self) -> u64 {
        self.admitted.load(Ordering::SeqCst)
    }

    /// Time left in the time budget.
    pub fn remaining(&self) -> Duration {
        self.limits
            .max_duration
            .saturating_sub(self.started.elapsed())
    }

    /// Admit one probe: wait for a concurrency slot, then count it against
    /// the probe budget. Fails, without sending anything, once a budget is
    /// spent. Cancel-safe: a probe dropped while waiting isn't counted.
    pub async fn admit(&self) -> Result<Admission, BudgetExhausted> {
        self.check_budgets()?;
        let slot = self.slots.acquire_arc().await;
        self.check_time()?;
        self.admitted
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < self.limits.max_probes).then_some(n + 1)
            })
            .map_err(|_| BudgetExhausted::Probes)?;
        Ok(Admission { _slot: slot })
    }

    /// Wait until the next request may be sent: past any `Retry-After` pause,
    /// then for the rate limit. Fails when the time budget ends first.
    pub async fn pace(&self) -> Result<Paced, BudgetExhausted> {
        loop {
            self.wait_out_pause().await?;
            let (limiter, generation) = {
                let pace = self.state();
                (pace.limiter.clone(), pace.generation)
            };
            limiter.until_ready().await;
            self.check_time()?;
            // A backoff while waiting replaced the limiter: wait for the new rate.
            if self.pause_left().is_none() && self.state().generation == generation {
                return Ok(Paced { generation });
            }
        }
    }

    /// Account for a response to a request sent after `paced`. A 429 halves
    /// the rate (once per rate: the 429s of other requests sent at the same
    /// rate don't halve it again) and pauses sending for its `Retry-After`.
    /// A pause that ends after the time budget stops sending for good.
    pub fn observe(&self, paced: Paced, status: u16, retry_after: Option<&str>) -> Option<Backoff> {
        if status != TOO_MANY_REQUESTS {
            return None;
        }
        let pause = retry_after.and_then(|value| parse_retry_after(value, SystemTime::now()));
        let mut pace = self.state();
        if paced.generation == pace.generation {
            pace.rate = halved(pace.rate);
            pace.limiter = Arc::new(limiter(pace.rate));
            // The next request waits a full interval at the new rate.
            let _ = pace.limiter.check();
            pace.generation += 1;
        }
        if let Some(pause) = pause {
            self.pause_for(&mut pace, pause);
        }
        Some(Backoff {
            rate: pace.rate,
            pause,
        })
    }

    fn pause_for(&self, pace: &mut Pace, pause: Duration) {
        let until = Instant::now().checked_add(pause);
        match until {
            Some(until) if pause < self.remaining() => {
                pace.resume_at = Some(pace.resume_at.map_or(until, |at| at.max(until)));
            }
            _ => pace.halted = true,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, Pace> {
        self.pace.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check_budgets(&self) -> Result<(), BudgetExhausted> {
        self.check_time()?;
        if self.admitted() >= self.limits.max_probes {
            return Err(BudgetExhausted::Probes);
        }
        Ok(())
    }

    fn check_time(&self) -> Result<(), BudgetExhausted> {
        if self.remaining().is_zero() {
            Err(BudgetExhausted::Duration)
        } else {
            Ok(())
        }
    }

    fn pause_left(&self) -> Option<Duration> {
        let resume_at = self.state().resume_at?;
        let left = resume_at.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// Sleep through the current pause, unless it outlasts the time budget.
    async fn wait_out_pause(&self) -> Result<(), BudgetExhausted> {
        if self.state().halted {
            return Err(BudgetExhausted::Duration);
        }
        while let Some(left) = self.pause_left() {
            if left >= self.remaining() {
                return Err(BudgetExhausted::Duration);
            }
            futures_timer::Delay::new(left).await;
        }
        Ok(())
    }
}

/// Evenly spaced requests: no bursts, even right after a backoff.
fn limiter(rate: NonZeroU32) -> DefaultDirectRateLimiter {
    RateLimiter::direct(Quota::per_second(rate).allow_burst(NonZeroU32::MIN))
}

/// Half the rate, rounding down, but at least 1 request per second.
fn halved(rate: NonZeroU32) -> NonZeroU32 {
    NonZeroU32::new(rate.get() / 2).unwrap_or(NonZeroU32::MIN)
}

/// A `Retry-After` value: delay seconds or an HTTP date (a date in the past
/// means no wait). `None` when it is neither.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;

    fn limits(rate: u32, concurrency: u32, max_probes: u64, max_duration: Duration) -> Limits {
        Limits {
            rate: NonZeroU32::new(rate).unwrap(),
            concurrency: NonZeroU32::new(concurrency).unwrap(),
            max_probes,
            max_duration,
        }
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn defaults_follow_the_spec() {
        let limits = Limits::default();
        assert_eq!(limits.rate.get(), 10);
        assert_eq!(limits.concurrency.get(), 4);
        assert_eq!(limits.max_probes, 2000);
        assert_eq!(limits.max_duration, Duration::from_secs(900));
    }

    #[test]
    fn retry_after_takes_seconds_or_a_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        let rows = [
            ("5", Some(Duration::from_secs(5))),
            (" 0 ", Some(Duration::ZERO)),
            // 2015-10-21 07:28:00 GMT is `now`; ten seconds later:
            (
                "Wed, 21 Oct 2015 07:28:10 GMT",
                Some(Duration::from_secs(10)),
            ),
            ("Wed, 21 Oct 2015 07:27:00 GMT", Some(Duration::ZERO)),
            ("-1", None),
            ("soon", None),
        ];
        for (value, expected) in rows {
            assert_eq!(parse_retry_after(value, now), expected, "{value}");
        }
    }

    #[test]
    fn halving_never_goes_below_one() {
        let rates: Vec<u32> = [10, 5, 3, 1]
            .into_iter()
            .map(|r| halved(NonZeroU32::new(r).unwrap()).get())
            .collect();
        assert_eq!(rates, [5, 2, 1, 1]);
    }

    #[test]
    fn the_probe_budget_admits_exactly_max_probes() {
        let governor = Governor::new(limits(1000, 8, 3, HOUR));
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(block_on(governor.admit()).unwrap());
        }
        assert_eq!(
            block_on(governor.admit()).unwrap_err(),
            BudgetExhausted::Probes
        );
        drop(held);
        // Freed slots don't refund the budget.
        assert_eq!(
            block_on(governor.admit()).unwrap_err(),
            BudgetExhausted::Probes
        );
        assert_eq!(governor.admitted(), 3);
    }

    #[test]
    fn a_spent_time_budget_admits_and_paces_nothing() {
        let governor = Governor::new(limits(1000, 1, 10, Duration::ZERO));
        assert_eq!(
            block_on(governor.admit()).unwrap_err(),
            BudgetExhausted::Duration
        );
        assert_eq!(
            block_on(governor.pace()).unwrap_err(),
            BudgetExhausted::Duration
        );
        assert_eq!(governor.admitted(), 0);
    }

    #[test]
    fn concurrency_slots_are_released_on_drop() {
        let governor = Governor::new(limits(1000, 1, 10, HOUR));
        let first = block_on(governor.admit()).unwrap();
        assert!(governor.slots.try_acquire().is_none());
        drop(first);
        let _second = block_on(governor.admit()).unwrap();
    }

    #[test]
    fn a_429_halves_the_rate_once_per_rate() {
        let governor = Governor::new(limits(10, 4, 10, HOUR));
        let a = block_on(governor.pace()).unwrap();
        let b = block_on(governor.pace()).unwrap();
        assert_eq!(governor.observe(a, 200, Some("5")), None);
        let backoff = governor.observe(a, 429, None).unwrap();
        assert_eq!(backoff.rate.get(), 5);
        assert_eq!(backoff.pause, None);
        // b was sent at 10 req/s too: its 429 doesn't halve again.
        assert_eq!(governor.observe(b, 429, None).unwrap().rate.get(), 5);
        let c = block_on(governor.pace()).unwrap();
        assert_eq!(governor.observe(c, 429, None).unwrap().rate.get(), 2);
        assert_eq!(governor.rate().get(), 2);
    }

    #[test]
    fn after_a_429_the_next_request_waits_the_halved_interval() {
        let governor = Governor::new(limits(4, 1, 10, HOUR));
        let paced = block_on(governor.pace()).unwrap();
        let start = Instant::now();
        governor.observe(paced, 429, None);
        block_on(governor.pace()).unwrap();
        // 2 req/s after the 429: 500 ms, not an immediate send.
        assert!(
            start.elapsed() >= Duration::from_millis(450),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_429_slows_requests_already_waiting_at_the_old_rate() {
        let governor = Governor::new(limits(4, 2, 10, HOUR));
        let first = block_on(governor.pace()).unwrap();
        let start = Instant::now();
        std::thread::scope(|s| {
            let waiting = s.spawn(|| block_on(governor.pace()).unwrap());
            std::thread::sleep(Duration::from_millis(50));
            governor.observe(first, 429, None);
            waiting.join().unwrap();
        });
        // The waiter would go at 250 ms at the old 4 req/s; at 2 req/s it
        // waits 500 ms from the 429.
        assert!(
            start.elapsed() >= Duration::from_millis(500),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn retry_after_pauses_pacing() {
        let governor = Governor::new(limits(1000, 1, 10, HOUR));
        let paced = block_on(governor.pace()).unwrap();
        let backoff = governor.observe(paced, 429, Some("1")).unwrap();
        assert_eq!(backoff.pause, Some(Duration::from_secs(1)));
        let start = Instant::now();
        block_on(governor.pace()).unwrap();
        assert!(
            start.elapsed() >= Duration::from_millis(900),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_pause_past_the_time_budget_stops_pacing() {
        let governor = Governor::new(limits(1000, 1, 10, Duration::from_secs(60)));
        let paced = block_on(governor.pace()).unwrap();
        governor.observe(paced, 429, Some("120"));
        let start = Instant::now();
        assert_eq!(
            block_on(governor.pace()).unwrap_err(),
            BudgetExhausted::Duration
        );
        assert!(start.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn a_huge_retry_after_stops_pacing_without_panicking() {
        let rows = [
            ("18446744073709551615", HOUR),
            // An unbounded time budget, where the pause can't be represented.
            ("18446744073709551615", Duration::MAX),
            ("9999999999", HOUR),
        ];
        for (retry_after, budget) in rows {
            let governor = Governor::new(limits(1000, 1, 10, budget));
            let paced = block_on(governor.pace()).unwrap();
            let backoff = governor.observe(paced, 429, Some(retry_after)).unwrap();
            assert_eq!(backoff.rate.get(), 500);
            assert_eq!(
                block_on(governor.pace()).unwrap_err(),
                BudgetExhausted::Duration,
                "{retry_after}"
            );
        }
    }

    #[test]
    fn a_probe_dropped_while_waiting_for_a_slot_is_not_counted() {
        let governor = Governor::new(limits(1000, 1, 2, HOUR));
        let first = block_on(governor.admit()).unwrap();
        let waiting = futures_lite::future::poll_once(governor.admit());
        assert!(block_on(waiting).is_none(), "admitted past the slot limit");
        assert_eq!(governor.admitted(), 1);
        drop(first);
        let _second = block_on(governor.admit()).unwrap();
        assert_eq!(governor.admitted(), 2);
    }

    #[test]
    fn a_spent_probe_budget_fails_without_waiting_for_a_slot() {
        let governor = Governor::new(limits(1000, 1, 1, HOUR));
        let _held = block_on(governor.admit()).unwrap();
        let attempt = futures_lite::future::poll_once(governor.admit());
        assert_eq!(
            block_on(attempt).map(|r| r.unwrap_err()),
            Some(BudgetExhausted::Probes)
        );
    }

    #[test]
    fn the_rate_limit_spaces_requests() {
        let governor = Governor::new(limits(20, 1, 10, HOUR));
        let start = Instant::now();
        for _ in 0..3 {
            block_on(governor.pace()).unwrap();
        }
        // The first is immediate; two more at 50 ms intervals.
        assert!(
            start.elapsed() >= Duration::from_millis(90),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn budget_errors_read_not_run() {
        assert_eq!(BudgetExhausted::Probes.to_string(), "not run: budget");
        assert_eq!(BudgetExhausted::Duration.to_string(), "not run: budget");
        assert_eq!(BudgetExhausted::Probes.detail(), "probe budget reached");
        assert_eq!(BudgetExhausted::Duration.detail(), "time budget reached");
    }
}
