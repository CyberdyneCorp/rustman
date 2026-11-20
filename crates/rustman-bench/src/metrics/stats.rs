//! The statistics of a set of requests: an HDR latency histogram (1 µs to
//! 10 min at 3 significant digits, so a fixed size whatever the request
//! count), counts, phase sums and bytes. Stats of the same shape merge, so
//! per-second windows add up to the totals exactly.

use std::collections::BTreeMap;

use hdrhistogram::Histogram;
use rustman_report::bench::{Checks, LatencySummary, PhaseSummary, StatsSummary};

use super::Sample;
use super::class::ErrorClass;

/// The highest latency recorded: 10 minutes, in microseconds.
pub const MAX_LATENCY_US: u64 = 600_000_000;
/// The precision of every histogram.
pub const SIGNIFICANT_DIGITS: u8 = 3;

/// A latency histogram with the bench's range and precision.
pub fn histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(1, MAX_LATENCY_US, SIGNIFICANT_DIGITS)
        .expect("valid histogram bounds")
}

/// Per-phase timings in milliseconds of one request (`http-requests`
/// timings); `None` for a phase it did not go through.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Phases {
    pub dns_ms: Option<f64>,
    pub connect_ms: Option<f64>,
    pub tls_ms: Option<f64>,
    pub ttfb_ms: Option<f64>,
    pub download_ms: Option<f64>,
}

impl Phases {
    fn values(&self) -> [Option<f64>; 5] {
        [
            self.dns_ms,
            self.connect_ms,
            self.tls_ms,
            self.ttfb_ms,
            self.download_ms,
        ]
    }
}

/// Sums and counts of each phase, for averages.
#[derive(Clone, Copy, Debug, Default)]
struct PhaseSums {
    sums: [f64; 5],
    counts: [u64; 5],
}

impl PhaseSums {
    fn record(&mut self, phases: &Phases) {
        for (i, value) in phases.values().into_iter().enumerate() {
            if let Some(ms) = value.filter(|ms| ms.is_finite() && *ms >= 0.0) {
                self.sums[i] += ms;
                self.counts[i] += 1;
            }
        }
    }

    fn add(&mut self, other: &Self) {
        for i in 0..5 {
            self.sums[i] += other.sums[i];
            self.counts[i] += other.counts[i];
        }
    }

    fn summary(&self) -> PhaseSummary {
        let avg = |i: usize| (self.counts[i] > 0).then(|| self.sums[i] / self.counts[i] as f64);
        PhaseSummary {
            dns_ms: avg(0),
            connect_ms: avg(1),
            tls_ms: avg(2),
            ttfb_ms: avg(3),
            download_ms: avg(4),
        }
    }
}

/// The statistics of one request file, or of every file together.
#[derive(Clone, Debug)]
pub struct Stats {
    latency: Histogram<u64>,
    requests: u64,
    errors: BTreeMap<ErrorClass, u64>,
    checks: Checks,
    phases: PhaseSums,
    bytes_sent: u64,
    bytes_received: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            latency: histogram(),
            requests: 0,
            errors: BTreeMap::new(),
            checks: Checks::default(),
            phases: PhaseSums::default(),
            bytes_sent: 0,
            bytes_received: 0,
        }
    }
}

impl Stats {
    pub fn record(&mut self, sample: &Sample) {
        let micros = u64::try_from(sample.latency.as_micros()).unwrap_or(u64::MAX);
        self.latency
            .saturating_record(micros.clamp(1, MAX_LATENCY_US));
        self.requests += 1;
        if let Some(class) = sample.error {
            *self.errors.entry(class).or_default() += 1;
        }
        self.checks.passed += sample.checks.passed;
        self.checks.failed += sample.checks.failed;
        self.phases.record(&sample.phases);
        self.bytes_sent += sample.bytes_sent;
        self.bytes_received += sample.bytes_received;
    }

    /// Add every request of `other` to these.
    pub fn add(&mut self, other: &Self) {
        // Both histograms share their bounds, so adding cannot fail.
        let _ = self.latency.add(&other.latency);
        self.requests += other.requests;
        for (class, n) in &other.errors {
            *self.errors.entry(*class).or_default() += n;
        }
        self.checks.passed += other.checks.passed;
        self.checks.failed += other.checks.failed;
        self.phases.add(&other.phases);
        self.bytes_sent += other.bytes_sent;
        self.bytes_received += other.bytes_received;
    }

    /// Forget every request, keeping the histogram's memory.
    pub fn reset(&mut self) {
        self.latency.reset();
        self.requests = 0;
        self.errors.clear();
        self.checks = Checks::default();
        self.phases = PhaseSums::default();
        self.bytes_sent = 0;
        self.bytes_received = 0;
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }

    pub fn errors(&self) -> u64 {
        self.errors.values().sum()
    }

    pub fn latency(&self) -> LatencySummary {
        latency_summary(&self.latency)
    }

    pub fn summary(&self) -> StatsSummary {
        StatsSummary {
            requests: self.requests,
            errors: self
                .errors
                .iter()
                .map(|(class, n)| (class.name().to_owned(), *n))
                .collect(),
            error_rate: ratio(self.errors(), self.requests),
            latency: self.latency(),
            checks: self.checks,
            phases: self.phases.summary(),
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
        }
    }
}

/// The percentiles of `histogram` in milliseconds; all 0 when empty.
pub fn latency_summary(histogram: &Histogram<u64>) -> LatencySummary {
    if histogram.is_empty() {
        return LatencySummary::default();
    }
    let ms = |micros: u64| micros as f64 / 1000.0;
    let at = |q: f64| ms(histogram.value_at_quantile(q));
    LatencySummary {
        min_ms: ms(histogram.min()),
        avg_ms: histogram.mean() / 1000.0,
        p50_ms: at(0.50),
        p90_ms: at(0.90),
        p95_ms: at(0.95),
        p99_ms: at(0.99),
        max_ms: ms(histogram.max()),
    }
}

pub fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The overall statistics and those of each request file.
#[derive(Clone, Debug)]
pub struct Window {
    pub overall: Stats,
    pub files: Vec<Stats>,
}

impl Window {
    pub fn new(files: usize) -> Self {
        Self {
            overall: Stats::default(),
            files: vec![Stats::default(); files],
        }
    }

    pub fn record(&mut self, sample: &Sample) {
        self.overall.record(sample);
        if let Some(stats) = self.files.get_mut(sample.file) {
            stats.record(sample);
        }
    }

    pub fn add(&mut self, other: &Self) {
        self.overall.add(&other.overall);
        for (mine, theirs) in self.files.iter_mut().zip(&other.files) {
            mine.add(theirs);
        }
    }

    pub fn reset(&mut self) {
        self.overall.reset();
        self.files.iter_mut().for_each(Stats::reset);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn sample(ms: u64, phases: Phases) -> Sample {
        Sample {
            latency: Duration::from_millis(ms),
            phases,
            ..Sample::default()
        }
    }

    #[test]
    fn phases_average_over_the_requests_that_had_them() {
        let mut stats = Stats::default();
        let fresh = Phases {
            dns_ms: Some(2.0),
            connect_ms: Some(4.0),
            ttfb_ms: Some(10.0),
            ..Phases::default()
        };
        let reused = Phases {
            ttfb_ms: Some(20.0),
            ..Phases::default()
        };
        stats.record(&sample(10, fresh));
        stats.record(&sample(20, reused));
        let phases = stats.summary().phases;
        assert_eq!(phases.dns_ms, Some(2.0));
        assert_eq!(phases.connect_ms, Some(4.0));
        assert_eq!(phases.ttfb_ms, Some(15.0));
        assert_eq!(phases.tls_ms, None);
    }

    #[test]
    fn windows_merge_and_reset() {
        let mut total = Window::new(1);
        let mut second = Window::new(1);
        second.record(&sample(5, Phases::default()));
        second.record(&Sample {
            error: Some(ErrorClass::Timeout),
            bytes_sent: 7,
            ..sample(9, Phases::default())
        });
        total.add(&second);
        second.reset();
        assert_eq!(second.overall.requests(), 0);
        assert_eq!(second.files[0].latency(), LatencySummary::default());
        let summary = total.files[0].summary();
        assert_eq!(summary.requests, 2);
        assert_eq!(summary.errors_named("timeout"), 1);
        assert_eq!(summary.bytes_sent, 7);
        assert!((summary.latency.max_ms - 9.0).abs() < 0.01);
    }
}
