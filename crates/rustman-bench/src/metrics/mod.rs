//! Bench metrics (design D3). Every finished request is a [`Sample`],
//! recorded into the current one-second window: HDR latency histograms per
//! request file and overall, error classes, assertion checks, phase
//! timings and bytes. Once a second, [`Metrics::tick`] turns the window
//! into a point of the time series and merges it into the totals, so the
//! end-of-run statistics are the per-second windows merged.
//!
//! Memory is bounded whatever the request count: histograms have a fixed
//! size and are reused, no latency sample is kept, the time series holds
//! one small point per second (at most [`MAX_SERIES`]) and error samples
//! are capped at [`MAX_SAMPLES_PER_CLASS`] per class.
//!
//! Latency is the transport time of each exchange (the request's `total`
//! timing), so a VU waiting on the rate limit is not counted as server
//! time. In the open model (arrival rate) it is measured from the
//! iteration's scheduled start instead (coordinated-omission correction,
//! [`LatencyKind::Corrected`]).

mod class;
mod stats;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use rustman_core::run::RequestRecord;
pub use rustman_report::bench::{
    BenchSecond, Checks, ErrorSample, FileSummary, LatencyKind, LatencySummary,
    MAX_SAMPLES_PER_CLASS, StatsSummary,
};
use serde::Serialize;

pub use class::{ErrorClass, describe};
pub use stats::{MAX_LATENCY_US, Phases, SIGNIFICANT_DIGITS, histogram, latency_summary};
use stats::{Window, ratio};

/// The most points the time series keeps: 24 hours of seconds.
pub const MAX_SERIES: usize = 86_400;

/// One finished request, as metrics see it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// The index of its file in the plan.
    pub file: usize,
    pub latency: Duration,
    pub error: Option<ErrorClass>,
    /// Assertions that passed and failed.
    pub checks: Checks,
    pub phases: Phases,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl Sample {
    /// The sample of `record`, the request of file `file`, with `latency`.
    pub fn of(file: usize, latency: Duration, record: &RequestRecord) -> Self {
        let passed = record.assertions.iter().filter(|a| a.passed()).count() as u64;
        let checks = Checks {
            passed,
            failed: record.assertions.len() as u64 - passed,
        };
        let base = Self {
            file,
            latency,
            error: ErrorClass::of(record),
            checks,
            ..Self::default()
        };
        let Some(exchange) = &record.exchange else {
            return base;
        };
        let t = &exchange.timings;
        let request = &exchange.request;
        let headers: usize = request
            .headers
            .iter()
            .map(|h| h.name.len() + h.value.len() + 4)
            .sum();
        // The request line, headers and body as recorded: an estimate.
        let sent = request.method.len()
            + request.url.len()
            + 12
            + headers
            + 2
            + request.body.as_ref().map_or(0, String::len);
        Self {
            phases: Phases {
                dns_ms: t.dns_ms,
                connect_ms: t.connect_ms,
                tls_ms: t.tls_ms,
                ttfb_ms: Some(t.ttfb_ms),
                download_ms: Some(t.download_ms),
            },
            bytes_sent: sent as u64,
            bytes_received: exchange.response.body_bytes,
            ..base
        }
    }
}

/// Collects the samples of a bench; shared by every VU.
#[derive(Debug)]
pub struct Metrics {
    kind: LatencyKind,
    files: Vec<String>,
    state: Mutex<State>,
    /// How long completed iterations took; a lock of its own, so VUs
    /// ending an iteration don't wait on request samples.
    iteration_durations: Mutex<Histogram<u64>>,
    active_vus: AtomicUsize,
    iterations: AtomicU64,
    dropped: AtomicU64,
    interrupted: AtomicU64,
    scheduled: AtomicU64,
}

#[derive(Debug)]
struct State {
    /// When load generation started ([`Metrics::start`]; until then, when
    /// the metrics were made).
    started: Instant,
    /// When it ended ([`Metrics::finish`]): the end of the rate's time base.
    ended: Option<Instant>,
    /// Every window merged so far.
    totals: Window,
    /// The second being recorded.
    current: Window,
    current_since: Instant,
    series: Vec<BenchSecond>,
    samples: Vec<ErrorSample>,
    sampled: BTreeMap<ErrorClass, usize>,
}

impl Metrics {
    /// Metrics for the request `files` (in plan order).
    pub fn new(kind: LatencyKind, files: Vec<String>) -> Self {
        let started = Instant::now();
        let state = State {
            started,
            ended: None,
            totals: Window::new(files.len()),
            current: Window::new(files.len()),
            current_since: started,
            series: Vec::new(),
            samples: Vec::new(),
            sampled: BTreeMap::new(),
        };
        Self {
            kind,
            files,
            state: Mutex::new(state),
            iteration_durations: Mutex::new(histogram()),
            active_vus: AtomicUsize::new(0),
            iterations: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            interrupted: AtomicU64::new(0),
            scheduled: AtomicU64::new(0),
        }
    }

    pub fn kind(&self) -> LatencyKind {
        self.kind
    }

    /// The request files, in plan order.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// When the bench started generating load.
    pub fn started(&self) -> Instant {
        self.state().started
    }

    /// Start the clock: the rates count from here, not from when the
    /// bench was prepared.
    pub(crate) fn start(&self) {
        let mut state = self.state();
        let now = Instant::now();
        state.started = now;
        state.current_since = now;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn record(&self, sample: &Sample) {
        self.state().current.record(sample);
    }

    /// Keep `message` (already redacted) as an error sample of `class` for
    /// request file `file`, unless that class has its 20 samples already.
    pub fn sample_error(&self, class: ErrorClass, file: usize, message: String) {
        let mut state = self.state();
        let at_ms = u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let taken = state.sampled.entry(class).or_default();
        if *taken >= MAX_SAMPLES_PER_CLASS {
            return;
        }
        *taken += 1;
        let file = self.files.get(file).cloned().unwrap_or_default();
        state.samples.push(ErrorSample {
            class: class.name().to_owned(),
            file,
            message,
            at_ms,
        });
    }

    /// Close the current second: append it to the time series, merge it
    /// into the totals and start the next one. Called once a second while
    /// the bench runs, and once at its end.
    pub fn tick(&self) -> BenchSecond {
        let active = u32::try_from(self.active_vus()).unwrap_or(u32::MAX);
        let mut guard = self.state();
        let state = &mut *guard;
        let now = Instant::now();
        let elapsed = now.duration_since(state.current_since).as_secs_f64();
        let window = &state.current.overall;
        let latency = window.latency();
        let point = BenchSecond {
            second: u32::try_from(state.series.len() + 1).unwrap_or(u32::MAX),
            requests: window.requests(),
            errors: window.errors(),
            rps: per_second(window.requests(), elapsed),
            p50_ms: latency.p50_ms,
            p95_ms: latency.p95_ms,
            p99_ms: latency.p99_ms,
            active_vus: active,
        };
        if state.series.len() < MAX_SERIES {
            state.series.push(point);
        }
        state.totals.add(&state.current);
        state.current.reset();
        state.current_since = now;
        point
    }

    /// Close the last, partial second when it saw any request, and stop
    /// the clock of the rates.
    pub fn finish(&self) {
        let pending = self.state().current.overall.requests() > 0;
        if pending {
            self.tick();
        }
        self.state().ended.get_or_insert_with(Instant::now);
    }

    /// The time series so far.
    pub fn series(&self) -> Vec<BenchSecond> {
        self.state().series.clone()
    }

    /// How many seconds have been closed (the time series may keep fewer).
    pub fn seconds(&self) -> usize {
        self.state().series.len()
    }

    /// The closed seconds from index `from` on, oldest first.
    pub fn seconds_since(&self, from: usize) -> Vec<BenchSecond> {
        let state = self.state();
        state
            .series
            .get(from..)
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }

    /// The last `n` closed seconds, oldest first.
    pub fn recent(&self, n: usize) -> Vec<BenchSecond> {
        let state = self.state();
        let from = state.series.len().saturating_sub(n);
        state.series[from..].to_vec()
    }

    /// The error samples so far, in the order they happened.
    pub fn error_samples(&self) -> Vec<ErrorSample> {
        self.state().samples.clone()
    }

    /// An iteration ran to its end after `took`.
    pub fn iteration_done(&self, took: Duration) {
        self.iterations.fetch_add(1, Ordering::SeqCst);
        let micros = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
        self.iteration_durations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .saturating_record(micros.clamp(1, MAX_LATENCY_US));
    }

    /// An iteration was cancelled before its end (graceful stop expired,
    /// or Ctrl+C).
    pub fn iteration_interrupted(&self) {
        self.interrupted.fetch_add(1, Ordering::SeqCst);
    }

    /// The open model scheduled an iteration.
    pub fn scheduled(&self) {
        self.scheduled.fetch_add(1, Ordering::SeqCst);
    }

    /// The open model found no free VU for a scheduled iteration.
    pub fn dropped(&self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }

    /// Active VUs: under the closed model, VUs running iterations or
    /// thinking between them; under the open model, VUs of the pool busy
    /// with an iteration.
    pub fn active_vus(&self) -> usize {
        self.active_vus.load(Ordering::SeqCst)
    }

    pub(crate) fn vu_started(&self) {
        self.active_vus.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn vu_stopped(&self) {
        self.active_vus.fetch_sub(1, Ordering::SeqCst);
    }

    /// Everything recorded so far: the totals with the current second.
    /// The lock is held only to copy them; merging happens after.
    pub fn summary(&self) -> BenchSummary {
        let (mut all, current, elapsed) = {
            let state = self.state();
            let end = state.ended.unwrap_or_else(Instant::now);
            let elapsed = end.saturating_duration_since(state.started);
            (state.totals.clone(), state.current.clone(), elapsed)
        };
        all.add(&current);
        let iteration_duration = {
            let durations = self
                .iteration_durations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            latency_summary(&durations)
        };
        let overall = all.overall.summary();
        BenchSummary {
            latency_kind: self.kind,
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
            iterations: self.iterations.load(Ordering::SeqCst),
            iteration_duration,
            interrupted: self.interrupted.load(Ordering::SeqCst),
            scheduled: self.scheduled.load(Ordering::SeqCst),
            dropped: self.dropped.load(Ordering::SeqCst),
            rps: per_second(overall.requests, rate_window(elapsed.as_secs_f64())),
            overall,
            files: self
                .files
                .iter()
                .zip(&all.files)
                .map(|(file, stats)| FileSummary {
                    file: file.clone(),
                    stats: stats.summary(),
                })
                .collect(),
        }
    }
}

/// The time base of a run's rates: its duration, but at least one second.
/// A rate over a shorter run says little and overstates a paced one: N
/// requests spaced by the `max_rps` limit span N - 1 intervals, so 2
/// requests at 200/s take about 5 ms, which would read as 400/s.
fn rate_window(secs: f64) -> f64 {
    secs.max(1.0)
}

fn per_second(count: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        0.0
    } else {
        count as f64 / secs
    }
}

/// What a bench measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BenchSummary {
    pub latency_kind: LatencyKind,
    pub elapsed_ms: f64,
    /// Iterations that ran to their end.
    pub iterations: u64,
    /// How long those iterations took, from their first request's start
    /// to their last one's end (think time excluded).
    pub iteration_duration: LatencySummary,
    /// Iterations cancelled before their end.
    pub interrupted: u64,
    /// Iterations the open model scheduled (0 for closed models).
    pub scheduled: u64,
    /// Scheduled iterations that found no free VU.
    pub dropped: u64,
    pub rps: f64,
    pub overall: StatsSummary,
    pub files: Vec<FileSummary>,
}

impl BenchSummary {
    /// The statistics of `file` (as listed in [`Self::files`]).
    pub fn file(&self, file: &str) -> Option<&StatsSummary> {
        self.files.iter().find(|f| f.file == file).map(|f| &f.stats)
    }

    /// Requests per second of `stats` over the run.
    pub fn rps_of(&self, stats: &StatsSummary) -> f64 {
        per_second(stats.requests, rate_window(self.elapsed_ms / 1000.0))
    }

    /// Errors over requests of every file together.
    pub fn error_rate(&self) -> f64 {
        let errors: u64 = self.overall.errors.values().sum();
        ratio(errors, self.overall.requests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(file: usize, micros: u64, error: Option<ErrorClass>) -> Sample {
        Sample {
            file,
            latency: Duration::from_micros(micros),
            error,
            bytes_received: 10,
            ..Sample::default()
        }
    }

    #[test]
    fn percentiles_come_from_the_histogram() {
        let metrics = Metrics::new(LatencyKind::Service, vec!["a".into(), "b".into()]);
        for ms in 1..=100 {
            metrics.record(&sample(0, ms * 1000, None));
        }
        metrics.record(&sample(1, 500_000, Some(ErrorClass::Status5xx)));
        let summary = metrics.summary();
        let a = &summary.files[0].stats;
        assert_eq!(a.requests, 100);
        assert!((a.latency.p50_ms - 50.0).abs() < 0.1, "{:?}", a.latency);
        assert!((a.latency.p99_ms - 99.0).abs() < 0.1, "{:?}", a.latency);
        assert!((a.latency.min_ms - 1.0).abs() < 0.01);
        assert_eq!(summary.files[1].stats.errors_named("5xx"), 1);
        assert_eq!(summary.overall.requests, 101);
        assert_eq!(summary.overall.bytes_received, 1010);
        assert!((summary.overall.error_rate - 1.0 / 101.0).abs() < 1e-9);
        assert_eq!(summary.file("b").unwrap().requests, 1);
    }

    #[test]
    fn an_empty_bench_has_zero_latency() {
        let summary = Metrics::new(LatencyKind::Corrected, Vec::new()).summary();
        assert_eq!(summary.overall.latency, LatencySummary::default());
        assert_eq!(summary.overall.error_rate, 0.0);
        assert_eq!(summary.latency_kind, LatencyKind::Corrected);
    }

    /// The exact percentile of `sorted` microseconds, in milliseconds: the
    /// value at rank ceil(q * n), as HDR counts ranks.
    fn exact(sorted: &[u64], q: f64) -> f64 {
        let rank = ((q * sorted.len() as f64).ceil() as usize).max(1);
        sorted[rank - 1] as f64 / 1000.0
    }

    /// Deterministic latency distributions, in microseconds.
    fn distributions() -> Vec<(&'static str, Vec<u64>)> {
        let uniform: Vec<u64> = (1..=100_000).map(|i| i * 7 % 250_000 + 1).collect();
        // Exponential with a 20 ms mean, by inverse transform.
        let exponential: Vec<u64> = (1..=50_000)
            .map(|i| {
                let u = f64::from(i) / 50_001.0;
                (-20_000.0 * (1.0 - u).ln()) as u64 + 1
            })
            .collect();
        // Mostly fast, with a slow tail of multi-second stalls.
        let bimodal: Vec<u64> = (0..40_000)
            .map(|i| {
                if i % 50 == 0 {
                    2_000_000 + i * 31
                } else {
                    800 + i % 4_000
                }
            })
            .collect();
        vec![
            ("uniform", uniform),
            ("exponential", exponential),
            ("bimodal", bimodal),
        ]
    }

    /// Task 4.1: percentiles of windows merged over several seconds stay
    /// within 0.1 % of the exact ones.
    #[test]
    fn merged_percentiles_are_within_a_tenth_of_a_percent() {
        for (name, values) in distributions() {
            let metrics = Metrics::new(LatencyKind::Service, vec!["f".into()]);
            for (i, micros) in values.iter().enumerate() {
                metrics.record(&sample(0, *micros, None));
                if i % 9_999 == 0 {
                    metrics.tick();
                }
            }
            let latency = metrics.summary().overall.latency;
            let mut sorted = values.clone();
            sorted.sort_unstable();
            let checks = [
                ("p50", latency.p50_ms, exact(&sorted, 0.50)),
                ("p90", latency.p90_ms, exact(&sorted, 0.90)),
                ("p95", latency.p95_ms, exact(&sorted, 0.95)),
                ("p99", latency.p99_ms, exact(&sorted, 0.99)),
                ("max", latency.max_ms, exact(&sorted, 1.0)),
                ("min", latency.min_ms, sorted[0] as f64 / 1000.0),
            ];
            for (label, measured, expected) in checks {
                let error = (measured - expected).abs() / expected;
                assert!(
                    error < 0.001,
                    "{name} {label}: {measured} vs {expected} ({:.4} %)",
                    error * 100.0
                );
            }
            let mean = values.iter().sum::<u64>() as f64 / values.len() as f64 / 1000.0;
            assert!((latency.avg_ms - mean).abs() / mean < 0.001, "{name} avg");
        }
    }

    #[test]
    fn ticks_build_the_series_and_keep_the_totals() {
        let metrics = Metrics::new(LatencyKind::Service, vec!["a".into()]);
        metrics.record(&sample(0, 10_000, None));
        metrics.record(&sample(0, 30_000, Some(ErrorClass::Timeout)));
        let first = metrics.tick();
        assert_eq!((first.second, first.requests, first.errors), (1, 2, 1));
        assert!((first.p99_ms - 30.0).abs() < 0.1, "{first:?}");
        metrics.finish();
        assert_eq!(
            metrics.series().len(),
            1,
            "an empty last second is not kept"
        );
        metrics.record(&sample(0, 5_000, None));
        metrics.finish();
        let series = metrics.series();
        assert_eq!(series.len(), 2);
        assert_eq!(series[1].second, 2);
        let summary = metrics.summary();
        assert_eq!(summary.overall.requests, 3);
        assert_eq!(summary.overall.errors_named("timeout"), 1);
    }

    /// The rates count from `start` to `finish`, over at least a second:
    /// two requests of a 5 ms run are not 400/s.
    #[test]
    fn rates_count_from_the_start_over_at_least_a_second() {
        let metrics = Metrics::new(LatencyKind::Service, vec!["a".into()]);
        std::thread::sleep(Duration::from_millis(50));
        metrics.start();
        metrics.record(&sample(0, 1_000, None));
        metrics.record(&sample(0, 1_000, None));
        metrics.finish();
        let summary = metrics.summary();
        assert!(summary.elapsed_ms < 50.0, "{summary:?}");
        assert!((summary.rps - 2.0).abs() < 1e-9, "{summary:?}");
        assert!((summary.rps_of(&summary.overall) - 2.0).abs() < 1e-9);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(metrics.summary().elapsed_ms, summary.elapsed_ms, "frozen");
    }

    #[test]
    fn error_samples_are_capped_per_class() {
        let metrics = Metrics::new(LatencyKind::Service, vec!["a.rman.yaml".into()]);
        for i in 0..30 {
            metrics.sample_error(ErrorClass::Status5xx, 0, format!("5xx {i}"));
        }
        metrics.sample_error(ErrorClass::Timeout, 0, "slow".into());
        let samples = metrics.error_samples();
        assert_eq!(samples.len(), MAX_SAMPLES_PER_CLASS + 1);
        assert_eq!(samples[0].message, "5xx 0");
        assert_eq!(samples[0].file, "a.rman.yaml");
        assert_eq!(samples.last().unwrap().class, "timeout");
    }

    #[test]
    fn iteration_durations_are_summarized() {
        let metrics = Metrics::new(LatencyKind::Service, vec!["a".into()]);
        for ms in [10, 20, 30, 40] {
            metrics.iteration_done(Duration::from_millis(ms));
        }
        let summary = metrics.summary();
        assert_eq!(summary.iterations, 4);
        let took = summary.iteration_duration;
        assert!((took.avg_ms - 25.0).abs() < 0.1, "{took:?}");
        assert!((took.max_ms - 40.0).abs() < 0.1, "{took:?}");
        assert!((took.min_ms - 10.0).abs() < 0.1, "{took:?}");
    }
}
