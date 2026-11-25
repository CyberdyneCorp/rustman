//! Soak mode (stress-testing "Soak mode"): `--rps` held for `--duration`,
//! with a linear trend fitted to the p95 and error rate of each trend
//! bucket (a minute). The p95 trend is a Theil-Sen slope (the median of the
//! pairwise slopes), so a few stalled minutes neither hide nor fake a
//! drift; the error rate, mostly zero, keeps a least-squares slope. A p95 slope above `--drift-threshold` (default 5% of
//! the median p95 of the first 10 minutes, per hour) is the `medium`
//! finding `latency-drift`; an error rate rising by more than one point per
//! hour is `error-drift`. Both hint at leaks: memory, connections, queues.
//!
//! [`SoakClock`] sets the bucket and the hour, so a test can run an
//! accelerated soak (seconds standing for minutes).

use std::time::Duration;

use rustman_report::bench::StatsSummary;
use rustman_report::exploration::Severity;

use crate::stress::{FlatSpec, StressFinding};

/// The soak profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoakSpec {
    /// Iterations per second.
    pub rps: f64,
    pub duration: Duration,
    /// The p95 slope that is a drift: this share of the baseline p95 per
    /// hour.
    pub drift_threshold: f64,
}

impl Default for SoakSpec {
    fn default() -> Self {
        Self {
            rps: 10.0,
            duration: Duration::from_secs(3600),
            drift_threshold: 0.05,
        }
    }
}

impl SoakSpec {
    pub fn flat(&self) -> FlatSpec {
        FlatSpec {
            rps: self.rps,
            duration: self.duration,
        }
    }
}

/// The time base of the drift analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoakClock {
    /// One point of the trend.
    pub bucket: Duration,
    /// What the slopes are expressed per.
    pub hour: Duration,
    /// The span whose median p95 is the baseline.
    pub baseline: Duration,
}

impl Default for SoakClock {
    fn default() -> Self {
        Self {
            bucket: Duration::from_secs(60),
            hour: Duration::from_secs(3600),
            baseline: Duration::from_secs(600),
        }
    }
}

/// The error-rate slope that is a drift: one point per hour.
pub const ERROR_DRIFT_PER_HOUR: f64 = 0.01;
/// Fewer buckets than this say nothing about a trend.
pub const MIN_BUCKETS: usize = 3;

/// What a soak measured.
#[derive(Clone, Debug, PartialEq)]
pub struct SoakResult {
    /// The p95 of each bucket, in milliseconds.
    pub p95_ms: Vec<f64>,
    /// The error rate of each bucket.
    pub error_rates: Vec<f64>,
    /// The median p95 of the baseline span.
    pub baseline_p95_ms: f64,
    /// Milliseconds per hour.
    pub p95_slope: f64,
    /// Fraction per hour.
    pub error_slope: f64,
    pub findings: Vec<StressFinding>,
}

/// Fit the trends of `buckets` (one per [`SoakClock::bucket`], in order).
pub fn analyze(buckets: &[StatsSummary], spec: &SoakSpec, clock: &SoakClock) -> SoakResult {
    let p95: Vec<f64> = buckets.iter().map(|b| b.latency.p95_ms).collect();
    let errors: Vec<f64> = buckets.iter().map(|b| b.error_rate).collect();
    let per_hour = clock.hour.as_secs_f64() / clock.bucket.as_secs_f64().max(f64::EPSILON);
    let baseline_len = (clock.baseline.as_secs_f64() / clock.bucket.as_secs_f64().max(f64::EPSILON))
        .ceil()
        .max(1.0) as usize;
    let baseline = median(&p95[..baseline_len.min(p95.len())]);
    let p95_slope = robust_slope(&p95) * per_hour;
    let error_slope = slope(&errors) * per_hour;
    let mut findings = Vec::new();
    if p95.len() >= MIN_BUCKETS {
        let limit = spec.drift_threshold * baseline;
        if p95_slope > limit {
            findings.push(StressFinding::new("latency-drift", Severity::Medium, format!(
                    "p95 rose by {:.1} ms per hour (from {:.1} ms to {:.1} ms; drift threshold {:.1} ms per hour)",
                    p95_slope,
                    p95.first().copied().unwrap_or_default(),
                    p95.last().copied().unwrap_or_default(),
                    limit
                )));
        }
        if error_slope > ERROR_DRIFT_PER_HOUR {
            findings.push(StressFinding::new(
                "error-drift",
                Severity::Medium,
                format!(
                    "the error rate rose by {:.2} points per hour (from {:.2}% to {:.2}%)",
                    error_slope * 100.0,
                    errors.first().copied().unwrap_or_default() * 100.0,
                    errors.last().copied().unwrap_or_default() * 100.0
                ),
            ));
        }
    }
    SoakResult {
        p95_ms: p95,
        error_rates: errors,
        baseline_p95_ms: baseline,
        p95_slope,
        error_slope,
        findings,
    }
}

/// The Theil-Sen slope of `values` against their index: the median of the
/// slopes between every pair of points; 0 with fewer than two values.
/// Unlike least squares, outliers up to about 29% of the points (stalls,
/// GC pauses) don't move it.
pub fn robust_slope(values: &[f64]) -> f64 {
    let mut slopes = Vec::with_capacity(values.len() * values.len().saturating_sub(1) / 2);
    for (i, a) in values.iter().enumerate() {
        for (j, b) in values.iter().enumerate().skip(i + 1) {
            slopes.push((b - a) / (j - i) as f64);
        }
    }
    median(&slopes)
}

/// The least-squares slope of `values` against their index; 0 with fewer
/// than two values.
pub fn slope(values: &[f64]) -> f64 {
    let n = values.len() as f64;
    if values.len() < 2 {
        return 0.0;
    }
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = values.iter().sum::<f64>() / n;
    let (num, den) = values
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(num, den), (i, y)| {
            let dx = i as f64 - mean_x;
            (num + dx * (y - mean_y), den + dx * dx)
        });
    if den == 0.0 { 0.0 } else { num / den }
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use rustman_report::bench::LatencySummary;

    use super::*;

    fn bucket(p95_ms: f64, error_rate: f64) -> StatsSummary {
        StatsSummary {
            requests: 600,
            error_rate,
            latency: LatencySummary {
                p95_ms,
                ..LatencySummary::default()
            },
            ..StatsSummary::default()
        }
    }

    fn rules(result: &SoakResult) -> Vec<&'static str> {
        result.findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn robust_slopes_ignore_a_few_outliers() {
        assert_eq!(robust_slope(&[1.0, 2.0, 3.0, 4.0]), 1.0);
        assert_eq!(robust_slope(&[5.0, 5.0, 5.0]), 0.0);
        assert_eq!(robust_slope(&[3.0]), 0.0);
        assert_eq!(robust_slope(&[]), 0.0);
        assert_eq!(robust_slope(&[2.0, 1.0, 0.0]), -1.0);
        // A flat series with one huge stall stays flat.
        assert_eq!(robust_slope(&[10.0, 10.0, 900.0, 10.0, 10.0, 10.0]), 0.0);
    }

    /// Regression: an accelerated soak on a loaded macOS runner stalled in
    /// two early buckets (633 and 845 ms). The least-squares slope of this
    /// series is negative, so the leak (42 ms rising to 211 ms) went
    /// unreported.
    #[test]
    fn stalled_buckets_do_not_hide_a_leak() {
        let p95 = [
            42.783, 57.215, 71.871, 633.343, 845.311, 118.079, 131.327, 145.407, 160.255, 202.623,
            197.119, 211.327,
        ];
        assert!(slope(&p95) < 0.0, "{}", slope(&p95));
        let buckets: Vec<StatsSummary> = p95.iter().map(|&p| bucket(p, 0.0)).collect();
        let clock = SoakClock {
            bucket: Duration::from_secs(1),
            hour: Duration::from_secs(6),
            baseline: Duration::from_secs(1),
        };
        let spec = SoakSpec {
            rps: 20.0,
            duration: Duration::from_secs(12),
            drift_threshold: 0.05,
        };
        let result = analyze(&buckets, &spec, &clock);
        assert_eq!(rules(&result), ["latency-drift"]);
        assert!(result.p95_slope > 30.0, "{}", result.p95_slope);
    }

    #[test]
    fn slopes_are_least_squares() {
        assert_eq!(slope(&[1.0, 2.0, 3.0, 4.0]), 1.0);
        assert_eq!(slope(&[5.0, 5.0, 5.0]), 0.0);
        assert_eq!(slope(&[3.0]), 0.0);
        assert!((slope(&[2.0, 1.0, 0.0]) + 1.0).abs() < 1e-12);
    }

    /// The spec's scenario: a 2-hour soak where p95 grows from 80 ms to
    /// 140 ms.
    #[test]
    fn a_rising_p95_is_latency_drift_in_ms_per_hour() {
        let buckets: Vec<StatsSummary> = (0..120)
            .map(|minute| bucket(80.0 + 60.0 * f64::from(minute) / 119.0, 0.0))
            .collect();
        let result = analyze(&buckets, &SoakSpec::default(), &SoakClock::default());
        assert_eq!(rules(&result), ["latency-drift"]);
        assert!(
            (result.p95_slope - 30.25).abs() < 0.01,
            "{}",
            result.p95_slope
        );
        assert_eq!(result.findings[0].severity, Severity::Medium);
        assert!(
            result.findings[0]
                .message
                .starts_with("p95 rose by 30.3 ms per hour"),
            "{}",
            result.findings[0].message
        );
    }

    /// Flat, noisy latency with a few spikes is no drift.
    #[test]
    fn noise_around_a_flat_p95_is_no_drift() {
        let noise = [0.0, 4.0, -3.0, 2.0, -5.0, 3.0, -1.0, 40.0, -2.0, 1.0];
        let buckets: Vec<StatsSummary> = (0..120)
            .map(|i| bucket(80.0 + noise[i % noise.len()], 0.001))
            .collect();
        let result = analyze(&buckets, &SoakSpec::default(), &SoakClock::default());
        assert!(result.findings.is_empty(), "{:?}", result.findings);
        assert_eq!(result.baseline_p95_ms, 80.5);
    }

    #[test]
    fn a_rising_error_rate_is_error_drift() {
        let buckets: Vec<StatsSummary> = (0..60)
            .map(|minute| bucket(80.0, 0.001 * f64::from(minute)))
            .collect();
        let result = analyze(&buckets, &SoakSpec::default(), &SoakClock::default());
        assert_eq!(rules(&result), ["error-drift"]);
        assert!((result.error_slope - 0.06).abs() < 1e-9);
    }

    #[test]
    fn the_threshold_and_the_clock_scale_the_verdict() {
        // +6 ms per hour on an 80 ms baseline: 7.5% per hour.
        let buckets: Vec<StatsSummary> = (0..60)
            .map(|minute| bucket(80.0 + 0.1 * f64::from(minute), 0.0))
            .collect();
        let clock = SoakClock::default();
        let strict = analyze(&buckets, &SoakSpec::default(), &clock);
        assert_eq!(rules(&strict), ["latency-drift"]);
        let lax = SoakSpec {
            drift_threshold: 0.1,
            ..SoakSpec::default()
        };
        assert!(analyze(&buckets, &lax, &clock).findings.is_empty());
        // Accelerated: one-second buckets standing for minutes.
        let fast = SoakClock {
            bucket: Duration::from_secs(1),
            hour: Duration::from_secs(60),
            baseline: Duration::from_secs(10),
        };
        let accelerated = analyze(&buckets, &SoakSpec::default(), &fast);
        assert!((accelerated.p95_slope - strict.p95_slope).abs() < 1e-9);
    }

    #[test]
    fn too_few_buckets_say_nothing() {
        let buckets = [bucket(10.0, 0.0), bucket(500.0, 0.5)];
        let result = analyze(&buckets, &SoakSpec::default(), &SoakClock::default());
        assert!(result.findings.is_empty());
    }
}
