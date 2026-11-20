//! The first failing endpoint (stress-testing "First failing endpoint"):
//! for a broken step, the request file whose SLO-relevant metric degraded
//! first, read from the per-file series of evaluated windows.
//!
//! A file degrades in a window when it fails an SLO scoped to it, or an
//! unscoped latency or error-rate SLO measured on its own requests. The
//! earliest window wins; within one window, the file furthest past its
//! limit. Client-saturated windows say nothing about the target and are
//! skipped.

use rustman_report::bench::StatsSummary;

use crate::stress::slo::{Slo, Slos};
use crate::stress::window::WindowVerdict;
use crate::threshold::{Metric, Op, Stat};

/// The request file that degraded first, with its numbers at the broken
/// step.
#[derive(Clone, Debug, PartialEq)]
pub struct FirstFailing {
    /// The request file, as the plan names it.
    pub file: String,
    /// The window where it first failed.
    pub window: usize,
    /// Its p95 and error rate in the last window of the broken step.
    pub p95_ms: f64,
    pub error_rate: f64,
}

impl FirstFailing {
    /// The file as reports name endpoints: its path without `.rman.yaml`
    /// and folders (`api/search.rman.yaml` is `search`).
    pub fn name(&self) -> &str {
        let id = self.file.strip_suffix(".rman.yaml").unwrap_or(&self.file);
        id.rsplit('/').next().unwrap_or(id)
    }
}

/// The first file to fail among `windows` (in run order), whose last one
/// is the broken step's last window.
pub fn first_failing(windows: &[&WindowVerdict], slos: &Slos) -> Option<FirstFailing> {
    let last = windows.last()?;
    let (window, file) = windows
        .iter()
        .filter(|w| !w.client_saturated)
        .find_map(|w| worst_file(w, slos).map(|file| (w.index, file)))?;
    let stats = last
        .stats
        .files
        .iter()
        .find(|f| f.file == file)
        .map(|f| &f.stats);
    Some(FirstFailing {
        p95_ms: stats.map_or(0.0, |s| s.latency.p95_ms),
        error_rate: stats.map_or(0.0, |s| s.error_rate),
        file,
        window,
    })
}

/// The file of `window` furthest past one of its SLOs, if any failed.
fn worst_file(window: &WindowVerdict, slos: &Slos) -> Option<String> {
    window
        .stats
        .files
        .iter()
        .filter(|f| f.stats.requests > 0)
        .filter_map(|f| {
            let excess = slos
                .for_file(&f.file)
                .filter_map(|slo| excess(slo, &f.stats))
                .reduce(f64::max)?;
            Some((f.file.clone(), excess))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(file, _)| file)
}

/// How far past its limit `stats` is for `slo` (measured over limit),
/// when it fails it.
fn excess(slo: &Slo, stats: &StatsSummary) -> Option<f64> {
    let threshold = &slo.threshold;
    let measured = measure(threshold.metric, stats)?;
    let holds = match threshold.op {
        Op::Lt => measured < threshold.value,
        Op::Le => measured <= threshold.value,
        Op::Gt => measured > threshold.value,
        Op::Ge => measured >= threshold.value,
    };
    if holds {
        return None;
    }
    let ratio = match threshold.op {
        Op::Lt | Op::Le => measured / threshold.value.max(f64::EPSILON),
        Op::Gt | Op::Ge => threshold.value / measured.max(f64::EPSILON),
    };
    Some(ratio)
}

/// The latency and error-rate metrics of one file's statistics.
fn measure(metric: Metric, stats: &StatsSummary) -> Option<f64> {
    let l = &stats.latency;
    Some(match metric {
        Metric::Latency(Stat::P50) => l.p50_ms,
        Metric::Latency(Stat::P90) => l.p90_ms,
        Metric::Latency(Stat::P95) => l.p95_ms,
        Metric::Latency(Stat::P99) => l.p99_ms,
        Metric::Latency(Stat::Max) => l.max_ms,
        Metric::Latency(Stat::Avg) => l.avg_ms,
        Metric::ErrorRate => stats.error_rate,
        Metric::Rps | Metric::Checks => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustman_report::bench::{FileSummary, LatencySummary};

    use super::*;
    use crate::stress::ModeKind;
    use crate::stress::window::WindowStats;

    const FILES: [&str; 3] = [
        "api/list.rman.yaml",
        "api/search.rman.yaml",
        "api/detail.rman.yaml",
    ];

    fn stats(p95_ms: f64, error_rate: f64) -> StatsSummary {
        StatsSummary {
            requests: 50,
            error_rate,
            latency: LatencySummary {
                p95_ms,
                ..LatencySummary::default()
            },
            ..StatsSummary::default()
        }
    }

    /// A window with the p95 of each file, in [`FILES`] order.
    fn window(index: usize, p95s: [f64; 3]) -> WindowVerdict {
        WindowVerdict {
            index,
            rate: 10.0,
            stats: WindowStats {
                length: Duration::from_secs(5),
                scheduled: 50,
                requests_per_iteration: 3,
                overall: stats(p95s.iter().copied().fold(0.0, f64::max), 0.0),
                files: FILES
                    .iter()
                    .zip(p95s)
                    .map(|(file, p95)| FileSummary {
                        file: (*file).to_owned(),
                        stats: stats(p95, 0.0),
                    })
                    .collect(),
                throttled: 0,
                throttled_5xx: 0,
                fuzzed: 0,
                in_flight: 0,
            },
            failures: Vec::new(),
            failure_mode: None,
            throttled: false,
            client_saturated: false,
        }
    }

    fn slos(flags: &[&str]) -> Slos {
        let files: Vec<String> = FILES.iter().map(|f| (*f).to_owned()).collect();
        let flags: Vec<Slo> = flags.iter().map(|s| s.parse().unwrap()).collect();
        Slos::new(&[], &flags, ModeKind::Breakpoint, &files).unwrap()
    }

    /// The spec's scenario: `search` crosses its p95 two windows before
    /// the others.
    #[test]
    fn the_file_that_crossed_first_is_named() {
        let windows = [
            window(0, [20.0, 30.0, 25.0]),
            window(1, [25.0, 400.0, 30.0]),
            window(2, [40.0, 600.0, 45.0]),
            window(3, [500.0, 900.0, 800.0]),
        ];
        let refs: Vec<&WindowVerdict> = windows.iter().collect();
        let first = first_failing(&refs, &slos(&["p95<300ms"])).unwrap();
        assert_eq!(first.name(), "search");
        assert_eq!(first.window, 1);
        assert_eq!(first.p95_ms, 900.0);
    }

    #[test]
    fn within_one_window_the_file_furthest_past_its_limit_wins() {
        let windows = [window(0, [350.0, 310.0, 20.0])];
        let refs: Vec<&WindowVerdict> = windows.iter().collect();
        let first = first_failing(&refs, &slos(&["p95<300ms"])).unwrap();
        assert_eq!(first.name(), "list");
    }

    #[test]
    fn scoped_slos_judge_their_file_and_saturated_windows_are_skipped() {
        let mut saturated = window(0, [20.0, 500.0, 20.0]);
        saturated.client_saturated = true;
        let windows = [saturated, window(1, [20.0, 30.0, 150.0])];
        let refs: Vec<&WindowVerdict> = windows.iter().collect();
        let scoped = slos(&["p95<1s", "file:detail p95<100ms"]);
        let first = first_failing(&refs, &scoped).unwrap();
        assert_eq!(first.name(), "detail");
        assert_eq!(first.window, 1);
        assert!(first_failing(&refs, &slos(&["p95<1s"])).is_none());
        assert!(first_failing(&[], &scoped).is_none());
    }
}
