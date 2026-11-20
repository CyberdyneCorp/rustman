//! SLO windows (stress-testing "SLO definitions"): what the stress run
//! measured over each window, per request file and overall, beside the
//! bench metrics' per-second series.
//!
//! Throttling (`429`, or `503` with `Retry-After`) is counted apart. Unless
//! `--treat-throttling-as-failure` is set, a throttled response is not an
//! error for the SLOs: a rate limiter doing its job is not a break.
//!
//! Memory is bounded: the recorder keeps the histograms of the open window
//! (and, for soak, of the open trend bucket) only; a closed window is a
//! small summary.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustman_core::run::RequestRecord;
use rustman_report::bench::{FileSummary, LatencyKind, LatencySummary, StatsSummary};

use crate::metrics::{BenchSummary, Sample, Window};
use crate::stress::classify::FailureMode;
use crate::stress::slo::SloFailure;

/// What one closed window measured.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowStats {
    pub length: Duration,
    /// Iterations scheduled (offered) during the window.
    pub scheduled: u64,
    /// Requests per iteration.
    pub requests_per_iteration: usize,
    pub overall: StatsSummary,
    pub files: Vec<FileSummary>,
    /// Throttled responses (`429`, or `503` with `Retry-After`).
    pub throttled: u64,
    /// Of those, the `503` ones (5xx errors when throttling fails).
    pub throttled_5xx: u64,
    /// Requests replaced by fuzzed variants: offered, but measured apart
    /// (the stats above are valid traffic only).
    pub fuzzed: u64,
    /// Iterations still in flight when the window closed (set by the run).
    pub in_flight: u64,
}

impl WindowStats {
    fn secs(&self) -> f64 {
        self.length.as_secs_f64().max(f64::EPSILON)
    }

    /// Valid requests offered per second (fuzzed ones are measured apart).
    pub fn offered_rps(&self) -> f64 {
        let offered = self.scheduled as f64 * self.requests_per_iteration as f64;
        (offered - self.fuzzed as f64).max(0.0) / self.secs()
    }

    /// Valid requests completed per second (compared with
    /// [`WindowStats::offered_rps`] by the classifier).
    pub fn achieved_rps(&self) -> f64 {
        self.overall.requests as f64 / self.secs()
    }

    /// Every request completed per second, valid and fuzzed: what the step
    /// table and the live view show next to the offered rate, which counts
    /// every request.
    pub fn completed_rps(&self) -> f64 {
        (self.overall.requests + self.fuzzed) as f64 / self.secs()
    }

    /// The share of throttled responses.
    pub fn throttled_share(&self) -> f64 {
        if self.overall.requests == 0 {
            0.0
        } else {
            self.throttled as f64 / self.overall.requests as f64
        }
    }

    /// Errors of `class` (`5xx`, `timeout`, `connect`, ...).
    pub fn errors(&self, class: &str) -> u64 {
        self.overall.errors.get(class).copied().unwrap_or(0)
    }

    /// The window as a bench summary, so the bench thresholds evaluate it.
    pub fn summary(&self) -> BenchSummary {
        BenchSummary {
            latency_kind: LatencyKind::Corrected,
            elapsed_ms: self.length.as_secs_f64() * 1000.0,
            iterations: 0,
            iteration_duration: LatencySummary::default(),
            interrupted: 0,
            scheduled: self.scheduled,
            dropped: 0,
            rps: self.achieved_rps(),
            overall: self.overall.clone(),
            files: self.files.clone(),
        }
    }
}

/// A closed window and what the run made of it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowVerdict {
    /// 0-based, in run order.
    pub index: usize,
    /// The iteration rate the controller set.
    pub rate: f64,
    pub stats: WindowStats,
    /// The SLOs it failed.
    pub failures: Vec<SloFailure>,
    /// The dominant symptom of a failed window.
    pub failure_mode: Option<FailureMode>,
    /// Enough throttled responses to call the target rate limited.
    pub throttled: bool,
    /// The client lagged behind its schedule: no verdict on the target.
    pub client_saturated: bool,
}

impl WindowVerdict {
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }
}

/// The share of throttled responses from which a window counts as rate
/// limited.
pub const THROTTLED_SHARE: f64 = 0.05;

/// How a response was throttled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throttled {
    /// `429 Too Many Requests`.
    TooManyRequests,
    /// `503 Service Unavailable` with `Retry-After`.
    Unavailable,
}

/// Whether `record` was throttled: `429 Too Many Requests`, or `503` with a
/// `Retry-After` header.
pub fn throttled(record: &RequestRecord) -> Option<Throttled> {
    let response = &record.exchange.as_ref()?.response;
    let retry_after = || {
        response
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("retry-after"))
    };
    match response.status {
        429 => Some(Throttled::TooManyRequests),
        503 if retry_after() => Some(Throttled::Unavailable),
        _ => None,
    }
}

struct Current {
    window: Window,
    scheduled: u64,
    throttled: u64,
    throttled_5xx: u64,
    fuzzed: u64,
    bucket: Window,
}

/// Records the samples of a stress run into the open window (and trend
/// bucket); shared by every VU.
pub(crate) struct Recorder {
    throttling_fails: bool,
    current: Mutex<Current>,
}

impl Recorder {
    pub fn new(files: usize, throttling_fails: bool) -> Self {
        Self {
            throttling_fails,
            current: Mutex::new(Current {
                window: Window::new(files),
                scheduled: 0,
                throttled: 0,
                throttled_5xx: 0,
                fuzzed: 0,
                bucket: Window::new(files),
            }),
        }
    }

    fn current(&self) -> MutexGuard<'_, Current> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The scheduler offered one iteration.
    pub fn scheduled(&self) {
        self.current().scheduled += 1;
    }

    /// A request of the window was replaced by a fuzzed variant.
    pub fn fuzzed(&self) {
        self.current().fuzzed += 1;
    }

    /// A finished request; a throttled one is not an error for the SLOs
    /// unless throttling counts as failure.
    pub fn record(&self, sample: &Sample, throttled: Option<Throttled>) {
        let sample = if throttled.is_some() && !self.throttling_fails {
            Sample {
                error: None,
                ..*sample
            }
        } else {
            *sample
        };
        let mut current = self.current();
        current.window.record(&sample);
        current.bucket.record(&sample);
        match throttled {
            Some(Throttled::TooManyRequests) => current.throttled += 1,
            Some(Throttled::Unavailable) => {
                current.throttled += 1;
                current.throttled_5xx += 1;
            }
            None => {}
        }
    }

    /// Close the open window, `length` long, and start the next one.
    pub fn close_window(
        &self,
        length: Duration,
        names: &[String],
        requests_per_iteration: usize,
    ) -> WindowStats {
        let mut current = self.current();
        let stats = WindowStats {
            length,
            scheduled: current.scheduled,
            requests_per_iteration,
            overall: current.window.overall.summary(),
            files: summaries(&current.window, names),
            throttled: current.throttled,
            throttled_5xx: current.throttled_5xx,
            fuzzed: current.fuzzed,
            in_flight: 0,
        };
        current.window.reset();
        current.scheduled = 0;
        current.fuzzed = 0;
        current.throttled = 0;
        current.throttled_5xx = 0;
        stats
    }

    /// Close the open trend bucket (soak) and start the next one.
    pub fn close_bucket(&self) -> StatsSummary {
        let mut current = self.current();
        let summary = current.bucket.overall.summary();
        current.bucket.reset();
        summary
    }
}

fn summaries(window: &Window, names: &[String]) -> Vec<FileSummary> {
    names
        .iter()
        .zip(&window.files)
        .map(|(file, stats)| FileSummary {
            file: file.clone(),
            stats: stats.summary(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ErrorClass;

    fn sample(file: usize, ms: u64, error: Option<ErrorClass>) -> Sample {
        Sample {
            file,
            latency: Duration::from_millis(ms),
            error,
            ..Sample::default()
        }
    }

    #[test]
    fn a_window_counts_offered_and_achieved_load_per_file() {
        let recorder = Recorder::new(2, false);
        for _ in 0..10 {
            recorder.scheduled();
        }
        for i in 0..8 {
            recorder.record(&sample(i % 2, 10, None), None);
        }
        recorder.record(&sample(1, 900, Some(ErrorClass::Status5xx)), None);
        let names = ["a.rman.yaml".to_owned(), "b.rman.yaml".to_owned()];
        let stats = recorder.close_window(Duration::from_secs(5), &names, 2);
        assert_eq!(stats.offered_rps(), 4.0);
        assert!((stats.achieved_rps() - 1.8).abs() < 1e-9);
        assert_eq!(stats.errors("5xx"), 1);
        assert_eq!(stats.files[1].stats.requests, 5);
        assert_eq!(stats.files[0].stats.errors.len(), 0);
        let next = recorder.close_window(Duration::from_secs(5), &names, 2);
        assert_eq!(next.overall.requests, 0);
        assert_eq!(next.scheduled, 0);
        for _ in 0..10 {
            recorder.scheduled();
        }
        recorder.fuzzed();
        recorder.fuzzed();
        let fuzzing = recorder.close_window(Duration::from_secs(5), &names, 2);
        assert_eq!(fuzzing.fuzzed, 2);
        assert_eq!(fuzzing.offered_rps(), 3.6);
        assert_eq!(fuzzing.achieved_rps(), 0.0);
        assert_eq!(fuzzing.completed_rps(), 0.4);
    }

    #[test]
    fn throttled_responses_are_no_error_unless_throttling_fails() {
        let throttled = sample(0, 2, Some(ErrorClass::Status4xx));
        let names = ["a".to_owned()];
        let ignoring = Recorder::new(1, false);
        ignoring.record(&throttled, Some(Throttled::TooManyRequests));
        ignoring.record(&sample(0, 2, None), None);
        let stats = ignoring.close_window(Duration::from_secs(1), &names, 1);
        assert_eq!(stats.overall.error_rate, 0.0);
        assert_eq!(stats.throttled, 1);
        assert_eq!(stats.throttled_share(), 0.5);
        let failing = Recorder::new(1, true);
        failing.record(&throttled, Some(Throttled::TooManyRequests));
        let stats = failing.close_window(Duration::from_secs(1), &names, 1);
        assert_eq!(stats.overall.error_rate, 1.0);
        assert_eq!(stats.throttled, 1);
    }

    #[test]
    fn trend_buckets_close_apart_from_windows() {
        let recorder = Recorder::new(1, false);
        recorder.record(&sample(0, 100, None), None);
        let names = ["a".to_owned()];
        recorder.close_window(Duration::from_secs(1), &names, 1);
        recorder.record(&sample(0, 300, None), None);
        let bucket = recorder.close_bucket();
        assert_eq!(bucket.requests, 2);
        assert!(bucket.latency.max_ms >= 299.0);
        assert_eq!(recorder.close_bucket().requests, 0);
    }
}
