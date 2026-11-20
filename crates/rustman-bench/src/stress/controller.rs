//! The mode controller (design D1): every mode is a [`Controller`] that sets
//! the target rate of the arrival-rate executor once per tick (a second)
//! and reads the verdict of each closed SLO window. The run loop owns the
//! clock: at each tick it closes the metrics' second and, every
//! [`Timing::window_ticks`] ticks, an SLO window, which it hands to
//! [`Controller::window_closed`] before asking [`Controller::rate_at`] for
//! the next tick's rate. A controller is pure state, so tests drive it with
//! a fake clock (tick numbers) and synthetic verdicts.

use std::time::Duration;

use crate::stress::FlatSpec;
use crate::stress::window::WindowVerdict;

/// The clock of a stress run. The defaults are those of the spec: the rate
/// changes once per second, SLOs are evaluated over 5-second windows and
/// the first window of a breakpoint step is a warm-up. Tests shorten them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How often the rate may change and the metrics close a point.
    pub tick: Duration,
    /// Ticks per SLO window.
    pub window_ticks: u32,
    /// Windows at the start of a breakpoint step that are not evaluated.
    pub warmup_windows: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            window_ticks: 5,
            warmup_windows: 1,
        }
    }
}

impl Timing {
    /// How long one SLO window lasts.
    pub fn window(&self) -> Duration {
        self.tick * self.window_ticks
    }

    /// `duration` in whole ticks, rounded up (at least one).
    pub fn ticks(&self, duration: Duration) -> u64 {
        let tick = self.tick.as_nanos().max(1);
        let ticks = duration.as_nanos().div_ceil(tick);
        u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
    }

    /// `duration` in whole windows, rounded up (at least one).
    pub fn windows(&self, duration: Duration) -> u32 {
        let ticks = self.ticks(duration);
        let windows = ticks.div_ceil(u64::from(self.window_ticks.max(1)));
        u32::try_from(windows).unwrap_or(u32::MAX).max(1)
    }
}

/// What a controller wants for the next tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Next {
    /// Offer this many iterations per second.
    Rate(f64),
    /// The mode is over: stop the load.
    Done,
}

/// A stress mode driving the arrival rate.
pub trait Controller: Send {
    /// The rate to offer during tick `tick` (0-based), asked at its start.
    fn rate_at(&mut self, tick: u64) -> Next;

    /// The SLO window that just closed (window index `verdict.index`).
    fn window_closed(&mut self, verdict: &WindowVerdict);
}

/// A flat rate for a number of ticks: soak and fuzz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flat {
    rate: f64,
    ticks: u64,
}

impl Flat {
    pub fn new(spec: FlatSpec, timing: &Timing) -> Self {
        Self {
            rate: spec.rps,
            ticks: timing.ticks(spec.duration),
        }
    }
}

impl Controller for Flat {
    fn rate_at(&mut self, tick: u64) -> Next {
        if tick < self.ticks {
            Next::Rate(self.rate)
        } else {
            Next::Done
        }
    }

    fn window_closed(&mut self, _verdict: &WindowVerdict) {}
}

/// The clean baseline phase of a run with fuzzing (stress-testing
/// "Valid-traffic impact"): `rate` for whole windows, then the mode's own
/// controller, which sees its ticks from 0 and none of the baseline's
/// windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Baseline {
    rate: f64,
    ticks: u64,
    windows: u32,
    closed: u32,
}

impl Baseline {
    /// `duration` rounded up to whole windows at `rate`.
    pub fn new(rate: f64, duration: Duration, timing: &Timing) -> Self {
        let windows = timing.windows(duration);
        Self {
            rate,
            ticks: u64::from(windows) * u64::from(timing.window_ticks.max(1)),
            windows,
            closed: 0,
        }
    }

    /// How many ticks the baseline lasts.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// How many windows the baseline lasts.
    pub fn windows(&self) -> u32 {
        self.windows
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The rate of tick `tick`: the baseline's, then `inner`'s.
    pub fn rate_at(&self, tick: u64, inner: &mut dyn Controller) -> Next {
        match tick.checked_sub(self.ticks) {
            None => Next::Rate(self.rate),
            Some(tick) => inner.rate_at(tick),
        }
    }

    /// A closed window: the baseline's own, else `inner`'s.
    pub fn window_closed(&mut self, verdict: &WindowVerdict, inner: &mut dyn Controller) {
        if self.closed < self.windows {
            self.closed += 1;
        } else {
            inner.window_closed(verdict);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_up_to_ticks_and_windows() {
        let timing = Timing::default();
        assert_eq!(timing.window(), Duration::from_secs(5));
        assert_eq!(timing.ticks(Duration::from_millis(2500)), 3);
        assert_eq!(timing.ticks(Duration::ZERO), 1);
        assert_eq!(timing.windows(Duration::from_secs(30)), 6);
        assert_eq!(timing.windows(Duration::from_secs(31)), 7);
        let fast = Timing {
            tick: Duration::from_millis(500),
            window_ticks: 2,
            warmup_windows: 1,
        };
        assert_eq!(fast.window(), Duration::from_secs(1));
        assert_eq!(fast.windows(Duration::from_secs(3)), 3);
    }

    #[test]
    fn a_baseline_runs_whole_windows_before_the_mode() {
        let timing = Timing::default();
        let mut baseline = Baseline::new(4.0, Duration::from_secs(7), &timing);
        assert_eq!(baseline.ticks(), 10);
        assert_eq!(baseline.windows(), 2);
        let spec = FlatSpec {
            rps: 25.0,
            duration: Duration::from_secs(2),
        };
        let mut flat = Flat::new(spec, &timing);
        let rates: Vec<Next> = (8..13).map(|t| baseline.rate_at(t, &mut flat)).collect();
        assert_eq!(
            rates,
            [
                Next::Rate(4.0),
                Next::Rate(4.0),
                Next::Rate(25.0),
                Next::Rate(25.0),
                Next::Done
            ]
        );
        struct Count(u32);
        impl Controller for Count {
            fn rate_at(&mut self, _tick: u64) -> Next {
                Next::Done
            }
            fn window_closed(&mut self, _verdict: &WindowVerdict) {
                self.0 += 1;
            }
        }
        let names = ["a".to_owned()];
        let stats = crate::stress::window::Recorder::new(1, false).close_window(
            Duration::from_secs(5),
            &names,
            1,
        );
        let verdict = WindowVerdict {
            index: 0,
            rate: 4.0,
            stats,
            failures: Vec::new(),
            failure_mode: None,
            throttled: false,
            client_saturated: false,
        };
        let mut inner = Count(0);
        for _ in 0..5 {
            baseline.window_closed(&verdict, &mut inner);
        }
        assert_eq!(inner.0, 3);
    }

    #[test]
    fn a_flat_controller_holds_its_rate_then_stops() {
        let spec = FlatSpec {
            rps: 25.0,
            duration: Duration::from_secs(3),
        };
        let mut flat = Flat::new(spec, &Timing::default());
        let rates: Vec<Next> = (0..4).map(|t| flat.rate_at(t)).collect();
        assert_eq!(
            rates,
            [
                Next::Rate(25.0),
                Next::Rate(25.0),
                Next::Rate(25.0),
                Next::Done
            ]
        );
    }
}
