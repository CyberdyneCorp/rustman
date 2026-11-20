//! The plain-text end-of-run summary of `rustman bench`: totals, then
//! latency percentiles and errors per request file and overall, then the
//! thresholds and why the run stopped early. It holds only counts, timings,
//! request file paths and threshold expressions, never request or response
//! content, so it has nothing to redact.

use std::fmt::Write;

use crate::bench::BenchOutcome;
use crate::metrics::{BenchSummary, LatencySummary, StatsSummary};

/// The whole end-of-run summary: [`render`], then the thresholds and the
/// abort marker.
pub fn render_outcome(outcome: &BenchOutcome) -> String {
    let mut out = render(&outcome.summary);
    if !outcome.thresholds.is_empty() {
        out.push_str("thresholds:\n");
        for result in &outcome.thresholds {
            let _ = writeln!(out, "  {}", result.line());
        }
    }
    if let Some(aborted) = outcome.aborted {
        let _ = writeln!(out, "aborted: {aborted}");
    }
    out
}

/// The summary as printed at the end of a bench.
pub fn render(summary: &BenchSummary) -> String {
    let mut out = String::new();
    let kind = summary.latency_kind.as_str();
    let _ = writeln!(
        out,
        "iterations: {} completed, {} interrupted",
        summary.iterations, summary.interrupted
    );
    if summary.iterations > 0 {
        let took = &summary.iteration_duration;
        let _ = writeln!(
            out,
            "iteration duration (ms): avg {:.2}, p95 {:.2}, max {:.2}",
            took.avg_ms, took.p95_ms, took.max_ms
        );
    }
    if summary.scheduled > 0 {
        let _ = writeln!(
            out,
            "scheduled: {} ({} dropped)",
            summary.scheduled, summary.dropped
        );
    }
    let _ = writeln!(
        out,
        "requests: {} ({:.1}/s), errors {}",
        summary.overall.requests,
        summary.rps,
        errors(&summary.overall)
    );
    let _ = writeln!(out, "latency ({kind}, ms):");
    let rows = summary
        .files
        .iter()
        .map(|f| (f.file.as_str(), &f.stats))
        .chain(std::iter::once(("overall", &summary.overall)));
    let rows: Vec<(&str, &StatsSummary)> = rows.collect();
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let _ = writeln!(
        out,
        "  {:<width$}  {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}  {:>8}  errors",
        "", "min", "avg", "p50", "p90", "p95", "p99", "max", "requests"
    );
    for (name, stats) in rows {
        let _ = writeln!(
            out,
            "  {name:<width$}  {}  {:>8}  {}",
            latency(&stats.latency),
            stats.requests,
            errors(stats)
        );
    }
    out
}

fn latency(l: &LatencySummary) -> String {
    [
        l.min_ms, l.avg_ms, l.p50_ms, l.p90_ms, l.p95_ms, l.p99_ms, l.max_ms,
    ]
    .iter()
    .map(|ms| format!("{ms:>8.2}"))
    .collect::<Vec<_>>()
    .join(" ")
}

/// `0.00%`, or `2.50% (4xx 3, timeout 2)`.
fn errors(stats: &StatsSummary) -> String {
    let rate = format!("{:.2}%", stats.error_rate * 100.0);
    if stats.errors.is_empty() {
        return rate;
    }
    let classes: Vec<String> = stats
        .errors
        .iter()
        .map(|(class, n)| format!("{class} {n}"))
        .collect();
    format!("{rate} ({})", classes.join(", "))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::metrics::{ErrorClass, LatencyKind, Metrics, Sample};

    #[test]
    fn the_summary_lists_each_file_and_the_overall_row() {
        let metrics = Metrics::new(
            LatencyKind::Service,
            vec!["login.rman.yaml".into(), "list.rman.yaml".into()],
        );
        for (file, ms, error) in [
            (0, 10, None),
            (0, 20, None),
            (1, 30, Some(ErrorClass::Status5xx)),
            (1, 40, None),
        ] {
            metrics.record(&Sample {
                file,
                latency: Duration::from_millis(ms),
                error,
                ..Sample::default()
            });
        }
        metrics.iteration_done(Duration::from_millis(30));
        metrics.iteration_done(Duration::from_millis(50));
        let text = render(&metrics.summary());
        assert!(
            text.starts_with("iterations: 2 completed, 0 interrupted\n"),
            "{text}"
        );
        assert!(text.contains("errors 25.00% (5xx 1)"), "{text}");
        assert!(
            text.contains("iteration duration (ms): avg 40.00, p95 50.0"),
            "{text}"
        );
        assert!(text.contains("latency (service, ms):"), "{text}");
        let login = text
            .lines()
            .find(|l| l.contains("login.rman.yaml"))
            .unwrap();
        assert!(
            login.contains("   10.00 ") && login.ends_with("0.00%"),
            "{login}"
        );
        let list = text.lines().find(|l| l.contains("list.rman.yaml")).unwrap();
        assert!(list.ends_with("50.00% (5xx 1)"), "{list}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("overall")));
        assert!(!text.contains("scheduled"), "{text}");
    }

    #[test]
    fn the_open_model_shows_drops_and_corrected_latency() {
        let metrics = Metrics::new(LatencyKind::Corrected, vec!["a.rman.yaml".into()]);
        metrics.scheduled();
        metrics.scheduled();
        metrics.dropped();
        let text = render(&metrics.summary());
        assert!(text.contains("scheduled: 2 (1 dropped)"), "{text}");
        assert!(text.contains("latency (corrected, ms):"), "{text}");
    }

    #[test]
    fn the_outcome_adds_thresholds_and_the_abort_marker() {
        use rustman_report::bench::{Aborted, ThresholdResult};

        let metrics = Metrics::new(LatencyKind::Service, vec!["a.rman.yaml".into()]);
        let outcome = BenchOutcome {
            summary: metrics.summary(),
            series: Vec::new(),
            thresholds: vec![ThresholdResult {
                expression: "p95<300ms".into(),
                passed: false,
                actual: "412ms".into(),
                abort: false,
            }],
            error_samples: Vec::new(),
            backoffs: 0,
            aborted: Some(Aborted::Threshold),
            generator_lag: Duration::ZERO,
            warnings: Vec::new(),
            started_at: String::new(),
            finished_at: String::new(),
        };
        let text = render_outcome(&outcome);
        assert!(
            text.ends_with("thresholds:\n  ✗ p95<300ms (412ms)\naborted: threshold\n"),
            "{text}"
        );
    }
}
