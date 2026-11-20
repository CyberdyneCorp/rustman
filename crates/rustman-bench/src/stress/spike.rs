//! Spike mode (stress-testing "Spike mode"): `--baseline-rps` for
//! `--baseline`, then `--spike-factor` × that rate for `--spike-duration`,
//! then the baseline rate again for another `--baseline`. The report gives
//! the errors during the spike and the recovery time: how long after the
//! spike the per-second p95 came back within 20% of the baseline p95 and
//! stayed there.

use std::time::Duration;

use rustman_report::bench::BenchSecond;

use crate::error::BenchError;
use crate::stress::controller::{Controller, Next, Timing};
use crate::stress::window::WindowVerdict;

/// The spike profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpikeSpec {
    /// Iterations per second before and after the spike.
    pub baseline_rps: f64,
    /// How long the baseline lasts, before and after the spike.
    pub baseline: Duration,
    pub spike_factor: f64,
    pub spike_duration: Duration,
}

impl Default for SpikeSpec {
    fn default() -> Self {
        Self {
            baseline_rps: 10.0,
            baseline: Duration::from_secs(30),
            spike_factor: 10.0,
            spike_duration: Duration::from_secs(30),
        }
    }
}

impl SpikeSpec {
    pub fn validate(&self) -> Result<(), BenchError> {
        let usage = |m: &str| Err(BenchError::Usage(format!("stress: {m}")));
        if !(self.baseline_rps.is_finite() && self.baseline_rps > 0.0) {
            return usage("--baseline-rps must be above 0");
        }
        if !(self.spike_factor.is_finite() && self.spike_factor > 1.0) {
            return usage("--spike-factor must be above 1");
        }
        Ok(())
    }

    /// The spike's rate.
    pub fn peak_rps(&self) -> f64 {
        self.baseline_rps * self.spike_factor
    }

    /// Baseline, spike and baseline again.
    pub fn total(&self) -> Duration {
        self.baseline * 2 + self.spike_duration
    }
}

/// The spike controller: a rate per tick from the profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spike {
    spec: SpikeSpec,
    baseline_ticks: u64,
    spike_ticks: u64,
}

impl Spike {
    pub fn new(spec: SpikeSpec, timing: &Timing) -> Self {
        Self {
            spec,
            baseline_ticks: timing.ticks(spec.baseline),
            spike_ticks: timing.ticks(spec.spike_duration),
        }
    }

    /// The first tick of the spike.
    pub fn spike_start(&self) -> u64 {
        self.baseline_ticks
    }

    /// The first tick after the spike.
    pub fn spike_end(&self) -> u64 {
        self.baseline_ticks + self.spike_ticks
    }
}

impl Controller for Spike {
    fn rate_at(&mut self, tick: u64) -> Next {
        if tick < self.spike_start() {
            Next::Rate(self.spec.baseline_rps)
        } else if tick < self.spike_end() {
            Next::Rate(self.spec.peak_rps())
        } else if tick < self.spike_end() + self.baseline_ticks {
            Next::Rate(self.spec.baseline_rps)
        } else {
            Next::Done
        }
    }

    fn window_closed(&mut self, _verdict: &WindowVerdict) {}
}

/// Seconds in a row within tolerance that count as recovered.
pub const RECOVERY_STREAK: usize = 3;
/// Recovered means a p95 within this factor of the baseline p95...
pub const RECOVERY_FACTOR: f64 = 1.2;
/// ...or within this many milliseconds of it, so the timer noise of a very
/// fast target (a 2 ms baseline) does not count as unrecovered.
pub const RECOVERY_SLACK_MS: f64 = 10.0;

/// What a spike did.
#[derive(Clone, Debug, PartialEq)]
pub struct SpikeResult {
    /// The median per-tick p95 of the baseline, in milliseconds.
    pub baseline_p95_ms: f64,
    /// Requests and errors during the spike.
    pub spike_requests: u64,
    pub spike_errors: u64,
    /// The highest per-tick p95 during or after the spike.
    pub peak_p95_ms: f64,
    /// How long after the spike p95 came back; `None` when it did not
    /// within the run.
    pub recovered_after: Option<Duration>,
}

/// Measure `series` (one point per tick) against the spike's schedule.
pub fn analyze(series: &[BenchSecond], spike: &Spike, timing: &Timing) -> SpikeResult {
    let start = usize::try_from(spike.spike_start()).unwrap_or(usize::MAX);
    let end = usize::try_from(spike.spike_end()).unwrap_or(usize::MAX);
    let before = series.get(..start.min(series.len())).unwrap_or_default();
    // The first tick of the run warms connections up.
    let warm = before.get(1..).filter(|b| !b.is_empty()).unwrap_or(before);
    let baseline = median(warm.iter().map(|s| s.p95_ms).collect());
    let during = series.get(start..end.min(series.len())).unwrap_or_default();
    let after = series.get(end.min(series.len())..).unwrap_or_default();
    let limit = (baseline * RECOVERY_FACTOR).max(baseline + RECOVERY_SLACK_MS);
    let recovered = recovery_tick(after, limit, RECOVERY_STREAK);
    SpikeResult {
        baseline_p95_ms: baseline,
        spike_requests: during.iter().map(|s| s.requests).sum(),
        spike_errors: during.iter().map(|s| s.errors).sum(),
        peak_p95_ms: during
            .iter()
            .chain(after)
            .map(|s| s.p95_ms)
            .fold(0.0, f64::max),
        recovered_after: recovered
            .map(|ticks| timing.tick * u32::try_from(ticks).unwrap_or(u32::MAX)),
    }
}

/// The index of the first point of `after` from which `streak` points in a
/// row (or every remaining one, at the end of the run) have a p95 within
/// `limit` and no error.
fn recovery_tick(after: &[BenchSecond], limit: f64, streak: usize) -> Option<usize> {
    let fine = |s: &BenchSecond| s.p95_ms <= limit && s.errors == 0 && s.requests > 0;
    (0..after.len()).find(|&at| {
        let run = &after[at..(at + streak).min(after.len())];
        !run.is_empty() && run.iter().all(fine)
    })
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(second: u32, p95_ms: f64, errors: u64) -> BenchSecond {
        BenchSecond {
            second,
            requests: 20,
            errors,
            rps: 20.0,
            p50_ms: p95_ms / 2.0,
            p95_ms,
            p99_ms: p95_ms,
            active_vus: 1,
        }
    }

    fn spike() -> (Spike, Timing) {
        let timing = Timing::default();
        let spec = SpikeSpec {
            baseline_rps: 20.0,
            baseline: Duration::from_secs(5),
            spike_factor: 10.0,
            spike_duration: Duration::from_secs(3),
        };
        (Spike::new(spec, &timing), timing)
    }

    #[test]
    fn the_schedule_is_baseline_spike_baseline() {
        let (mut spike, _) = spike();
        let rates: Vec<Next> = (0..14).map(|t| spike.rate_at(t)).collect();
        let base = Next::Rate(20.0);
        let peak = Next::Rate(200.0);
        assert_eq!(
            rates,
            [
                base,
                base,
                base,
                base,
                base,
                peak,
                peak,
                peak,
                base,
                base,
                base,
                base,
                base,
                Next::Done
            ]
        );
    }

    /// The spec's scenario: p95 returns within 20% of the baseline 14 s
    /// after the spike.
    #[test]
    fn recovery_is_counted_from_the_end_of_the_spike() {
        let timing = Timing::default();
        let spec = SpikeSpec {
            baseline_rps: 20.0,
            baseline: Duration::from_secs(20),
            spike_factor: 10.0,
            spike_duration: Duration::from_secs(5),
        };
        let spike = Spike::new(spec, &timing);
        let mut series = Vec::new();
        for s in 0..20 {
            series.push(point(s + 1, 100.0, 0));
        }
        for s in 20..25 {
            series.push(point(s + 1, 2000.0, 3));
        }
        for s in 25..45 {
            let p95 = if s < 39 { 400.0 } else { 115.0 };
            series.push(point(s + 1, p95, 0));
        }
        let result = analyze(&series, &spike, &timing);
        assert_eq!(result.baseline_p95_ms, 100.0);
        assert_eq!(result.spike_errors, 15);
        assert_eq!(result.spike_requests, 100);
        assert_eq!(result.peak_p95_ms, 2000.0);
        assert_eq!(result.recovered_after, Some(Duration::from_secs(14)));
    }

    #[test]
    fn a_single_good_second_or_errors_are_not_recovery() {
        let after = [
            point(1, 500.0, 0),
            point(2, 100.0, 0),
            point(3, 500.0, 0),
            point(4, 100.0, 1),
            point(5, 100.0, 0),
            point(6, 105.0, 0),
            point(7, 110.0, 0),
        ];
        assert_eq!(recovery_tick(&after, 120.0, 3), Some(4));
        assert_eq!(recovery_tick(&after[..3], 120.0, 3), None);
        // At the end of the run, the remaining good seconds count.
        assert_eq!(recovery_tick(&after[..6], 120.0, 3), Some(4));
    }

    #[test]
    fn a_fast_baseline_tolerates_timer_noise() {
        let (spike, timing) = spike();
        let mut series: Vec<BenchSecond> = (0..5).map(|s| point(s + 1, 2.0, 0)).collect();
        series.extend((5..8).map(|s| point(s + 1, 300.0, 0)));
        series.extend((8..13).map(|s| point(s + 1, 6.0, 0)));
        let result = analyze(&series, &spike, &timing);
        assert_eq!(result.recovered_after, Some(Duration::ZERO));
    }

    #[test]
    fn a_spike_that_never_recovers_says_so() {
        let (spike, timing) = spike();
        let mut series: Vec<BenchSecond> = (0..5).map(|s| point(s + 1, 50.0, 0)).collect();
        series.extend((5..13).map(|s| point(s + 1, 900.0, 0)));
        assert_eq!(analyze(&series, &spike, &timing).recovered_after, None);
    }

    #[test]
    fn bad_profiles_are_usage_errors() {
        let bad = SpikeSpec {
            spike_factor: 1.0,
            ..SpikeSpec::default()
        };
        assert!(bad.validate().is_err());
        let bad = SpikeSpec {
            baseline_rps: 0.0,
            ..SpikeSpec::default()
        };
        assert!(bad.validate().is_err());
        assert_eq!(SpikeSpec::default().total(), Duration::from_secs(90));
    }
}
