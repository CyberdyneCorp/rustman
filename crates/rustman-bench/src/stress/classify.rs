//! Failure-mode classification (design D3, stress-testing "Failure mode
//! classification"): the dominant symptom of a failed window.
//!
//! - When an error SLO failed (`error_rate`, `checks`), the most frequent
//!   error kind: `server-errors` (5xx), `timeouts`, `connection` (refused,
//!   reset, DNS, TLS) or `throttled` (`429`, or `503` with `Retry-After`,
//!   counted as errors only with `--treat-throttling-as-failure`).
//! - Otherwise (a latency SLO failed): `saturation` when the achieved rate
//!   is below 90% of the offered rate while latency is up, else `latency`.

use std::fmt;

use crate::stress::slo::SloFailure;
use crate::stress::window::WindowStats;
use crate::threshold::Metric;

/// How a window broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureMode {
    Latency,
    ServerErrors,
    Timeouts,
    Connection,
    Saturation,
    Throttled,
}

impl FailureMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Latency => "latency",
            Self::ServerErrors => "server-errors",
            Self::Timeouts => "timeouts",
            Self::Connection => "connection",
            Self::Saturation => "saturation",
            Self::Throttled => "throttled",
        }
    }
}

impl fmt::Display for FailureMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Achieved below this share of the offered rate is saturation.
pub const SATURATED_SHARE: f64 = 0.9;
/// Latency counts as up when p95 exceeds the reference by this factor.
const LATENCY_UP: f64 = 1.2;

/// The failure mode of `window`, which failed `failures`. `reference_p95`
/// is the lowest p95 the run saw in a good window (0 when none yet);
/// `throttling_fails` is `--treat-throttling-as-failure`.
pub fn classify(
    window: &WindowStats,
    failures: &[SloFailure],
    reference_p95: f64,
    throttling_fails: bool,
) -> Option<FailureMode> {
    if failures.is_empty() {
        return None;
    }
    let error_slo = failures
        .iter()
        .any(|f| matches!(f.metric, Metric::ErrorRate | Metric::Checks));
    if error_slo && let Some(mode) = dominant_error(window, throttling_fails) {
        return Some(mode);
    }
    Some(latency_mode(window, reference_p95))
}

/// The most frequent error kind, if any error happened.
fn dominant_error(window: &WindowStats, throttling_fails: bool) -> Option<FailureMode> {
    let (throttled, throttled_5xx) = if throttling_fails {
        (window.throttled, window.throttled_5xx)
    } else {
        (0, 0)
    };
    // A throttled 503 is counted as throttled, not as a server error.
    let server = window.errors("5xx").saturating_sub(throttled_5xx);
    let counts = [
        (FailureMode::ServerErrors, server),
        (FailureMode::Timeouts, window.errors("timeout")),
        (
            FailureMode::Connection,
            window.errors("connect") + window.errors("tls"),
        ),
        (FailureMode::Throttled, throttled),
    ];
    counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(mode, _)| mode)
}

fn latency_mode(window: &WindowStats, reference_p95: f64) -> FailureMode {
    let behind = window.achieved_rps() < window.offered_rps() * SATURATED_SHARE;
    let latency_up = window.overall.latency.p95_ms > reference_p95 * LATENCY_UP;
    if behind && latency_up {
        FailureMode::Saturation
    } else {
        FailureMode::Latency
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use rustman_report::bench::{LatencySummary, StatsSummary};

    use super::*;
    use crate::threshold::Stat;

    fn failure(metric: Metric) -> SloFailure {
        SloFailure {
            expression: String::new(),
            metric,
            actual: String::new(),
            file: None,
        }
    }

    const LATENCY: Metric = Metric::Latency(Stat::P95);

    /// A 1-second window: `offered` and `achieved` requests per second,
    /// p95 and errors by class.
    fn window(offered: u64, achieved: u64, p95_ms: f64, errors: &[(&str, u64)]) -> WindowStats {
        let errors: BTreeMap<String, u64> =
            errors.iter().map(|(c, n)| ((*c).to_owned(), *n)).collect();
        WindowStats {
            length: Duration::from_secs(1),
            scheduled: offered,
            requests_per_iteration: 1,
            overall: StatsSummary {
                requests: achieved,
                errors,
                latency: LatencySummary {
                    p95_ms,
                    ..LatencySummary::default()
                },
                ..StatsSummary::default()
            },
            files: Vec::new(),
            throttled: 0,
            throttled_5xx: 0,
            fuzzed: 0,
            in_flight: 0,
        }
    }

    #[test]
    fn a_passing_window_has_no_failure_mode() {
        assert_eq!(classify(&window(10, 10, 5.0, &[]), &[], 5.0, false), None);
    }

    #[test]
    fn error_slos_name_the_dominant_error() {
        let errors = [failure(Metric::ErrorRate)];
        let rows: [(&[(&str, u64)], FailureMode); 4] = [
            (&[("5xx", 30), ("timeout", 2)], FailureMode::ServerErrors),
            (&[("5xx", 3), ("timeout", 20)], FailureMode::Timeouts),
            (
                &[("connect", 9), ("tls", 9), ("5xx", 10)],
                FailureMode::Connection,
            ),
            (&[("4xx", 50)], FailureMode::Latency),
        ];
        for (counts, expected) in rows {
            let w = window(100, 100, 5.0, counts);
            assert_eq!(
                classify(&w, &errors, 5.0, false),
                Some(expected),
                "{counts:?}"
            );
        }
    }

    #[test]
    fn throttling_is_a_failure_mode_only_when_it_counts() {
        let mut w = window(100, 100, 5.0, &[("4xx", 30)]);
        w.throttled = 30;
        let errors = [failure(Metric::ErrorRate)];
        assert_eq!(
            classify(&w, &errors, 5.0, true),
            Some(FailureMode::Throttled)
        );
        // 503 + Retry-After are throttled, not server errors.
        let mut w = window(100, 100, 5.0, &[("5xx", 30)]);
        w.throttled = 30;
        w.throttled_5xx = 30;
        assert_eq!(
            classify(&w, &errors, 5.0, true),
            Some(FailureMode::Throttled)
        );
    }

    /// The spec's scenario: 300 offered, 210 achieved, rising p95.
    #[test]
    fn falling_behind_with_rising_latency_is_saturation() {
        let latency = [failure(LATENCY)];
        let w = window(300, 210, 900.0, &[]);
        assert_eq!(
            classify(&w, &latency, 100.0, false),
            Some(FailureMode::Saturation)
        );
        let keeping_up = window(300, 295, 900.0, &[]);
        assert_eq!(
            classify(&keeping_up, &latency, 100.0, false),
            Some(FailureMode::Latency)
        );
        let flat_latency = window(300, 210, 105.0, &[]);
        assert_eq!(
            classify(&flat_latency, &latency, 100.0, false),
            Some(FailureMode::Latency)
        );
    }
}
