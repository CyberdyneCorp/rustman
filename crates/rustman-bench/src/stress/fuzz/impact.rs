//! Valid-traffic impact (stress-testing "Valid-traffic impact"): a run with
//! fuzzing first runs a clean baseline phase at the same rate; valid
//! traffic while fuzzing is then compared with it. A valid p95 more than
//! 50% above the baseline (and more than 10 ms above it, so a fast
//! target's jitter does not count), or a valid error rate more than 1
//! point above it, is the `high` finding `fuzz-degrades-valid-traffic`.

use rustman_report::exploration::Severity;

use crate::stress::StressFinding;
use crate::stress::window::WindowStats;

pub const RULE: &str = "fuzz-degrades-valid-traffic";

/// How much higher the valid p95 may be than the baseline's.
pub const P95_RISE: f64 = 0.5;
/// The smallest p95 rise, in milliseconds, that counts as slower.
pub const P95_FLOOR_MS: f64 = 10.0;
/// How many points higher the valid error rate may be.
pub const ERROR_RISE: f64 = 0.01;

/// Valid traffic over some windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Valid {
    pub requests: u64,
    /// The request-weighted mean of the windows' p95, in milliseconds.
    pub p95_ms: f64,
    pub error_rate: f64,
}

impl Valid {
    /// Valid traffic over `windows`; `None` without requests.
    pub fn of<'a>(windows: impl IntoIterator<Item = &'a WindowStats>) -> Option<Self> {
        let (mut requests, mut weighted, mut errors) = (0u64, 0.0, 0.0);
        for w in windows {
            let n = w.overall.requests;
            requests += n;
            weighted += w.overall.latency.p95_ms * n as f64;
            errors += w.overall.error_rate * n as f64;
        }
        (requests > 0).then(|| Self {
            requests,
            p95_ms: weighted / requests as f64,
            error_rate: errors / requests as f64,
        })
    }
}

/// The finding when valid traffic while fuzzing (`fuzzing`) degraded
/// against the `baseline`.
pub fn judge(baseline: &Valid, fuzzing: &Valid) -> Option<StressFinding> {
    let rise = fuzzing.p95_ms - baseline.p95_ms;
    let slower = rise > baseline.p95_ms * P95_RISE && rise > P95_FLOOR_MS;
    let failing = fuzzing.error_rate - baseline.error_rate > ERROR_RISE;
    if !(slower || failing) {
        return None;
    }
    let message = format!(
        "valid traffic degraded while fuzzing: p95 {:.0}ms (baseline {:.0}ms), error rate {:.2}% (baseline {:.2}%)",
        fuzzing.p95_ms,
        baseline.p95_ms,
        fuzzing.error_rate * 100.0,
        baseline.error_rate * 100.0
    );
    Some(StressFinding::new(RULE, Severity::High, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(p95_ms: f64, error_rate: f64) -> Valid {
        Valid {
            requests: 100,
            p95_ms,
            error_rate,
        }
    }

    #[test]
    fn thresholds_of_the_spec() {
        let table = [
            (valid(100.0, 0.0), valid(260.0, 0.0), true),
            (valid(100.0, 0.0), valid(150.0, 0.0), false),
            (valid(100.0, 0.0), valid(151.0, 0.0), true),
            (valid(100.0, 0.01), valid(100.0, 0.02), false),
            (valid(100.0, 0.01), valid(100.0, 0.025), true),
            (valid(100.0, 0.0), valid(90.0, 0.0), false),
        ];
        for (baseline, fuzzing, fires) in table {
            assert_eq!(
                judge(&baseline, &fuzzing).is_some(),
                fires,
                "{baseline:?} -> {fuzzing:?}"
            );
        }
    }

    #[test]
    fn jitter_on_a_fast_target_is_not_a_finding() {
        assert!(judge(&valid(11.0, 0.0), &valid(19.0, 0.0)).is_none());
        assert!(judge(&valid(12.0, 0.0), &valid(16.0, 0.0)).is_none());
        assert!(judge(&valid(11.0, 0.0), &valid(22.0, 0.0)).is_some());
    }

    #[test]
    fn the_finding_names_both_values() {
        let finding = judge(&valid(100.0, 0.0), &valid(260.0, 0.0)).unwrap();
        assert_eq!(finding.rule, RULE);
        assert_eq!(finding.severity, Severity::High);
        assert!(
            finding.message.contains("p95 260ms (baseline 100ms)"),
            "{}",
            finding.message
        );
    }
}
