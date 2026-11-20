//! The kill switch (design D7, stress-testing "Kill switch"): the target
//! is **down** when, for the kill window (5 s), connection errors exceed
//! half of the attempts, or timeouts do, or when the health request gets
//! no answer 3 times in a row. Load then stops at once.
//!
//! The switch is pure state: the run loop hands it what each tick saw
//! ([`Beat`], counted by every VU into a shared [`Pulse`]) and each health
//! probe's result, so tests drive it with a fake clock.
//!
//! A tick in which nothing completed neither starts nor ends a streak:
//! requests that hang until their timeout complete in bursts.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::metrics::ErrorClass;
use crate::stress::classify::FailureMode;

/// The share of attempts above which connection errors (or timeouts)
/// count against the target.
pub const DOWN_SHARE: f64 = 0.5;

/// How long the error shares must stay above [`DOWN_SHARE`].
pub const KILL_WINDOW: Duration = Duration::from_secs(5);

/// Health requests without an answer, in a row, that mean down.
pub const HEALTH_FAILURES: u32 = 3;

/// What one tick saw: requests completed, and how many of them failed to
/// connect (refused, reset, TLS) or timed out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Beat {
    pub attempts: u64,
    pub connection: u64,
    pub timeouts: u64,
}

impl Beat {
    fn over(&self, part: u64) -> bool {
        self.attempts > 0 && part as f64 > self.attempts as f64 * DOWN_SHARE
    }
}

/// The counters every VU adds to; the run loop takes them once per tick.
#[derive(Debug, Default)]
pub struct Pulse {
    attempts: AtomicU64,
    connection: AtomicU64,
    timeouts: AtomicU64,
}

impl Pulse {
    /// One completed request, failed with `error` or not.
    pub fn record(&self, error: Option<ErrorClass>) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        match error {
            Some(ErrorClass::Connect | ErrorClass::Tls) => {
                self.connection.fetch_add(1, Ordering::Relaxed);
            }
            Some(ErrorClass::Timeout) => {
                self.timeouts.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// What was recorded since the last take.
    pub fn take(&self) -> Beat {
        Beat {
            attempts: self.attempts.swap(0, Ordering::Relaxed),
            connection: self.connection.swap(0, Ordering::Relaxed),
            timeouts: self.timeouts.swap(0, Ordering::Relaxed),
        }
    }
}

/// Why the target was declared down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownReason {
    /// Connection errors above half of the attempts for the kill window.
    Connection,
    /// Timeouts above half of the attempts for the kill window.
    Timeouts,
    /// The health request got no answer [`HEALTH_FAILURES`] times in a row.
    Health(FailureMode),
}

impl DownReason {
    /// The failure mode of the step the target went down in.
    pub fn failure_mode(self) -> FailureMode {
        match self {
            Self::Connection => FailureMode::Connection,
            Self::Timeouts => FailureMode::Timeouts,
            Self::Health(mode) => mode,
        }
    }
}

impl fmt::Display for DownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection => write!(
                f,
                "connection errors above {:.0}% for {}s",
                DOWN_SHARE * 100.0,
                KILL_WINDOW.as_secs()
            ),
            Self::Timeouts => write!(
                f,
                "timeouts above {:.0}% for {}s",
                DOWN_SHARE * 100.0,
                KILL_WINDOW.as_secs()
            ),
            Self::Health(_) => write!(
                f,
                "the health request got no answer {HEALTH_FAILURES} times in a row"
            ),
        }
    }
}

/// Load stopped because the target was down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Killed {
    /// The offered rate when it went down, in requests per second.
    pub rps: f64,
    pub reason: DownReason,
}

impl fmt::Display for Killed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "killed: target down at {} req/s ({})",
            crate::stress::format_rate(self.rps),
            self.reason
        )
    }
}

/// What the dashboard shows of the switch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KillState {
    /// Connection errors over attempts in the last tick with any.
    pub connection_share: f64,
    /// Timeouts over attempts in the last tick with any.
    pub timeout_share: f64,
    /// Health requests without an answer, in a row.
    pub health_failures: u32,
    /// Set once the target was declared down.
    pub down: Option<DownReason>,
}

/// The kill switch of one run.
#[derive(Clone, Debug)]
pub struct KillSwitch {
    /// Ticks the shares must stay high.
    needed: u64,
    connection_since: Option<u64>,
    timeout_since: Option<u64>,
    state: KillState,
}

impl KillSwitch {
    /// A switch over ticks of `tick`, tripping after `window` of high
    /// error shares.
    pub fn new(window: Duration, tick: Duration) -> Self {
        let tick = tick.as_nanos().max(1);
        let needed = u64::try_from(window.as_nanos().div_ceil(tick)).unwrap_or(u64::MAX);
        Self {
            needed: needed.max(1),
            connection_since: None,
            timeout_since: None,
            state: KillState::default(),
        }
    }

    pub fn state(&self) -> KillState {
        self.state
    }

    /// Tick `tick` (0-based) saw `beat`. Returns the reason once the target
    /// is down.
    pub fn tick(&mut self, tick: u64, beat: Beat) -> Option<DownReason> {
        if beat.attempts > 0 {
            let attempts = beat.attempts as f64;
            self.state.connection_share = beat.connection as f64 / attempts;
            self.state.timeout_share = beat.timeouts as f64 / attempts;
            streak(&mut self.connection_since, beat.over(beat.connection), tick);
            streak(&mut self.timeout_since, beat.over(beat.timeouts), tick);
        }
        let lasted = |since: Option<u64>| since.is_some_and(|s| tick + 1 - s >= self.needed);
        if lasted(self.connection_since) {
            return self.trip(DownReason::Connection);
        }
        if lasted(self.timeout_since) {
            return self.trip(DownReason::Timeouts);
        }
        None
    }

    /// A health probe got an answer (`None`) or failed with `error`.
    pub fn health(&mut self, error: Option<ErrorClass>) -> Option<DownReason> {
        let Some(error) = error else {
            self.state.health_failures = 0;
            return None;
        };
        self.state.health_failures += 1;
        if self.state.health_failures < HEALTH_FAILURES {
            return None;
        }
        let mode = if error == ErrorClass::Timeout {
            FailureMode::Timeouts
        } else {
            FailureMode::Connection
        };
        self.trip(DownReason::Health(mode))
    }

    fn trip(&mut self, reason: DownReason) -> Option<DownReason> {
        self.state.down.get_or_insert(reason);
        self.state.down
    }
}

/// Start or end a streak of bad ticks.
fn streak(since: &mut Option<u64>, bad: bool, tick: u64) {
    if bad {
        since.get_or_insert(tick);
    } else {
        *since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(attempts: u64, connection: u64, timeouts: u64) -> Beat {
        Beat {
            attempts,
            connection,
            timeouts,
        }
    }

    fn switch() -> KillSwitch {
        KillSwitch::new(KILL_WINDOW, Duration::from_secs(1))
    }

    /// Refusals from tick 3 on: down at the end of the fifth bad tick, so
    /// within 6 s of the first refusal however late in its tick it came.
    #[test]
    fn five_seconds_of_refusals_trip_it() {
        let mut kill = switch();
        for tick in 0..3 {
            assert_eq!(kill.tick(tick, beat(10, 0, 0)), None);
        }
        for tick in 3..7 {
            assert_eq!(kill.tick(tick, beat(10, 9, 0)), None, "tick {tick}");
        }
        assert_eq!(kill.tick(7, beat(10, 9, 0)), Some(DownReason::Connection));
        assert_eq!(kill.state().down, Some(DownReason::Connection));
        assert!((kill.state().connection_share - 0.9).abs() < 1e-9);
    }

    #[test]
    fn a_good_tick_ends_the_streak_and_an_empty_one_does_not() {
        let mut kill = switch();
        for tick in 0..4 {
            kill.tick(tick, beat(4, 3, 0));
        }
        assert_eq!(kill.tick(4, beat(4, 1, 0)), None);
        for tick in 5..9 {
            assert_eq!(kill.tick(tick, beat(4, 0, 3)), None);
        }
        // Nothing completed: the timeout streak goes on.
        assert_eq!(kill.tick(9, Beat::default()), Some(DownReason::Timeouts));
    }

    #[test]
    fn half_is_not_above_half() {
        let mut kill = switch();
        for tick in 0..20 {
            assert_eq!(kill.tick(tick, beat(10, 5, 5)), None);
        }
    }

    #[test]
    fn three_unanswered_health_requests_in_a_row_trip_it() {
        let mut kill = switch();
        assert_eq!(kill.health(Some(ErrorClass::Connect)), None);
        assert_eq!(kill.health(Some(ErrorClass::Connect)), None);
        assert_eq!(kill.health(None), None);
        assert_eq!(kill.state().health_failures, 0);
        kill.health(Some(ErrorClass::Timeout));
        kill.health(Some(ErrorClass::Timeout));
        let down = kill.health(Some(ErrorClass::Timeout)).unwrap();
        assert_eq!(down, DownReason::Health(FailureMode::Timeouts));
        assert_eq!(down.failure_mode(), FailureMode::Timeouts);
        assert_eq!(
            down.to_string(),
            "the health request got no answer 3 times in a row"
        );
    }

    #[test]
    fn the_window_is_counted_in_ticks() {
        let mut kill = KillSwitch::new(KILL_WINDOW, Duration::from_millis(500));
        for tick in 0..9 {
            assert_eq!(kill.tick(tick, beat(2, 2, 0)), None);
        }
        assert_eq!(kill.tick(9, beat(2, 2, 0)), Some(DownReason::Connection));
    }

    #[test]
    fn a_kill_names_the_rate() {
        let killed = Killed {
            rps: 180.0,
            reason: DownReason::Connection,
        };
        assert_eq!(
            killed.to_string(),
            "killed: target down at 180 req/s (connection errors above 50% for 5s)"
        );
    }

    #[test]
    fn the_pulse_counts_connection_errors_and_timeouts() {
        let pulse = Pulse::default();
        pulse.record(None);
        pulse.record(Some(ErrorClass::Connect));
        pulse.record(Some(ErrorClass::Tls));
        pulse.record(Some(ErrorClass::Timeout));
        pulse.record(Some(ErrorClass::Status5xx));
        assert_eq!(pulse.take(), beat(5, 2, 1));
        assert_eq!(pulse.take(), Beat::default());
    }
}
