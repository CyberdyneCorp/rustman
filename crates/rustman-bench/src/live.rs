//! The live view of a running bench, for the dashboards (add-load-testing
//! section 6): a [`Live`] handle reads the shared [`Metrics`] without
//! touching the VUs, and each [`Live::snapshot`] is a self-contained
//! picture of the run so far. Front ends draw it (the TTY dashboard), print
//! it ([`progress_line`], every 10 seconds when stdout is not a terminal)
//! or stream it (the viz companion's `bench_tick`).
//!
//! Like the end-of-run summary, a snapshot holds counts, timings, VU
//! numbers, request file paths and threshold expressions only, never
//! request or response content, so it has nothing to redact.
//!
//! Reading a snapshot locks the metrics just long enough to copy the
//! totals (the merge and percentiles are computed after the lock is
//! released), so a VU recording a request waits at most for that copy,
//! however slowly a front end draws.

use std::sync::Arc;
use std::time::Duration;

use rustman_report::bench::{BenchSecond, ThresholdResult};
use tokio_util::sync::CancellationToken;

use crate::bench::INTERRUPT_DRAIN;
use crate::metrics::{BenchSummary, Metrics};
use crate::threshold::{Threshold, evaluate_all};

/// How many seconds the sparklines of a snapshot cover.
pub const SPARK_SECONDS: usize = 120;

/// How often, at most, a dashboard redraws (4 times a second).
pub const FRAME: Duration = Duration::from_millis(250);

/// How often a progress line is printed when stdout is not a terminal.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// A handle on a bench's live metrics; cheap to clone and `Send`.
#[derive(Clone, Debug)]
pub struct Live {
    metrics: Arc<Metrics>,
    thresholds: Vec<Threshold>,
    planned: Option<Duration>,
    interrupt: Option<CancellationToken>,
}

/// What a front end says on a first Ctrl+C:
/// `interrupted: no new iterations; waiting up to 10s for those in flight (Ctrl+C again to cancel them)`.
pub fn interrupt_notice() -> String {
    format!(
        "interrupted: no new iterations; waiting up to {}s for those in flight (Ctrl+C again to cancel them)",
        INTERRUPT_DRAIN.as_secs()
    )
}

/// The run so far.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub elapsed: Duration,
    /// How long load still runs, when the profile has a fixed duration.
    pub remaining: Option<Duration>,
    pub active_vus: u32,
    /// Requests per second of the last closed second (0 before the first).
    pub rps: f64,
    /// Every request so far, per file and overall.
    pub summary: BenchSummary,
    /// The last [`SPARK_SECONDS`] seconds, oldest first.
    pub recent: Vec<BenchSecond>,
    /// Every threshold against the run so far.
    pub thresholds: Vec<ThresholdResult>,
    /// Ctrl+C was pressed: no new iteration starts.
    pub interrupted: bool,
}

impl Live {
    /// A view of `metrics` with the bench's `thresholds`, for a profile
    /// that runs `planned` (`None` when it ends on an iteration count).
    pub fn new(
        metrics: Arc<Metrics>,
        thresholds: Vec<Threshold>,
        planned: Option<Duration>,
    ) -> Self {
        Self {
            metrics,
            thresholds,
            planned,
            interrupt: None,
        }
    }

    /// Report `interrupt` (the bench's first-Ctrl+C token) in snapshots.
    #[must_use]
    pub fn with_interrupt(mut self, interrupt: CancellationToken) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    /// The seconds closed so far.
    pub fn seconds(&self) -> usize {
        self.metrics.seconds()
    }

    /// The closed seconds from the `from`-th on (0-based), oldest first.
    pub fn seconds_since(&self, from: usize) -> Vec<BenchSecond> {
        self.metrics.seconds_since(from)
    }

    /// How long load still runs, when the profile has a fixed duration.
    pub fn remaining(&self, elapsed: Duration) -> Option<Duration> {
        self.planned.map(|p| p.saturating_sub(elapsed))
    }

    /// The run so far.
    pub fn snapshot(&self) -> Snapshot {
        let summary = self.metrics.summary();
        let recent = self.metrics.recent(SPARK_SECONDS);
        let elapsed = self.metrics.started().elapsed();
        Snapshot {
            elapsed,
            remaining: self.remaining(elapsed),
            active_vus: u32::try_from(self.metrics.active_vus()).unwrap_or(u32::MAX),
            rps: recent.last().map_or(0.0, |s| s.rps),
            thresholds: evaluate_all(&self.thresholds, &summary),
            summary,
            recent,
            interrupted: self
                .interrupt
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled),
        }
    }
}

impl Snapshot {
    /// Errors over requests of the whole run so far.
    pub fn error_rate(&self) -> f64 {
        self.summary.error_rate()
    }

    /// Thresholds currently breached.
    pub fn breached(&self) -> usize {
        self.thresholds.iter().filter(|t| !t.passed).count()
    }
}

/// The one-line progress of a bench, as printed every 10 seconds when
/// stdout is not a terminal:
/// `bench 10s/30s: 5 VUs, 98.0 req/s, 980 requests, p50 1.20ms, p95 3.40ms, p99 5.10ms, errors 0.00%, thresholds 1/2 passing`.
pub fn progress_line(snapshot: &Snapshot) -> String {
    let elapsed = clock(snapshot.elapsed);
    let time = match snapshot.remaining {
        Some(remaining) => format!("{elapsed}/{}", clock(snapshot.elapsed + remaining)),
        None => elapsed,
    };
    let latency = &snapshot.summary.overall.latency;
    let mut line = format!(
        "bench {time}: {} VUs, {:.1} req/s, {} requests, p50 {:.2}ms, p95 {:.2}ms, p99 {:.2}ms, errors {:.2}%",
        snapshot.active_vus,
        snapshot.rps,
        snapshot.summary.overall.requests,
        latency.p50_ms,
        latency.p95_ms,
        latency.p99_ms,
        snapshot.error_rate() * 100.0,
    );
    if !snapshot.thresholds.is_empty() {
        let total = snapshot.thresholds.len();
        let passing = total - snapshot.breached();
        line.push_str(&format!(", thresholds {passing}/{total} passing"));
    }
    line
}

/// Whole seconds, as `45s` or `2m05s`.
pub fn clock(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{ErrorClass, LatencyKind, Sample};

    fn live(planned: Option<Duration>, thresholds: &[&str]) -> Live {
        let metrics = Arc::new(Metrics::new(
            LatencyKind::Service,
            vec!["a.rman.yaml".into(), "b.rman.yaml".into()],
        ));
        let thresholds = thresholds.iter().map(|t| t.parse().unwrap()).collect();
        Live::new(metrics, thresholds, planned)
    }

    fn record(live: &Live, file: usize, ms: u64, error: Option<ErrorClass>) {
        live.metrics().record(&Sample {
            file,
            latency: Duration::from_millis(ms),
            error,
            ..Sample::default()
        });
    }

    #[test]
    fn a_snapshot_reads_the_run_so_far() {
        let live = live(
            Some(Duration::from_secs(3600)),
            &["p95<1s", "error_rate<1%"],
        );
        record(&live, 0, 10, None);
        record(&live, 1, 20, Some(ErrorClass::Status5xx));
        live.metrics().tick();
        record(&live, 0, 30, None);
        let snap = live.snapshot();
        assert_eq!(snap.summary.overall.requests, 3, "the open second counts");
        assert_eq!(snap.recent.len(), 1);
        assert_eq!(snap.recent[0].requests, 2);
        assert!(snap.rps > 0.0);
        assert!(snap.remaining.unwrap() > Duration::from_secs(3500));
        assert_eq!(snap.thresholds.len(), 2);
        assert_eq!(snap.breached(), 1, "{:?}", snap.thresholds);
        assert!((snap.error_rate() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn an_iteration_limited_bench_has_no_remaining_time() {
        let snap = live(None, &[]).snapshot();
        assert_eq!(snap.remaining, None);
        assert_eq!(snap.rps, 0.0, "no second closed yet");
    }

    #[test]
    fn the_sparklines_keep_only_the_recent_seconds() {
        let live = live(None, &[]);
        for _ in 0..SPARK_SECONDS + 5 {
            live.metrics().tick();
        }
        let snap = live.snapshot();
        assert_eq!(snap.recent.len(), SPARK_SECONDS);
        assert_eq!(snap.recent[0].second, 6);
        assert_eq!(live.seconds(), SPARK_SECONDS + 5);
        assert_eq!(live.seconds_since(SPARK_SECONDS + 3).len(), 2);
    }

    #[test]
    fn the_clock_shows_whole_seconds() {
        assert_eq!(clock(Duration::ZERO), "0s");
        assert_eq!(clock(Duration::from_millis(59_999)), "59s");
        assert_eq!(clock(Duration::from_secs(125)), "2m05s");
    }

    #[test]
    fn the_progress_line_has_time_rate_latency_errors_and_thresholds() {
        let live = live(Some(Duration::from_secs(30)), &["p95<1s"]);
        record(&live, 0, 10, None);
        let mut snap = live.snapshot();
        snap.elapsed = Duration::from_millis(10_400);
        snap.remaining = Some(Duration::from_millis(19_600));
        snap.active_vus = 5;
        snap.rps = 98.0;
        let line = progress_line(&snap);
        assert!(
            line.starts_with("bench 10s/30s: 5 VUs, 98.0 req/s, 1 requests, p50 10."),
            "{line}"
        );
        assert!(
            line.ends_with("errors 0.00%, thresholds 1/1 passing"),
            "{line}"
        );
        snap.remaining = None;
        snap.thresholds.clear();
        let line = progress_line(&snap);
        assert!(line.starts_with("bench 10s: "), "{line}");
        assert!(line.ends_with("errors 0.00%"), "{line}");
    }

    #[test]
    fn the_interrupt_notice_names_the_drain_and_snapshots_follow_the_token() {
        let notice = interrupt_notice();
        let drain = format!("waiting up to {}s ", INTERRUPT_DRAIN.as_secs());
        assert!(notice.contains(&drain), "{notice}");
        let token = CancellationToken::new();
        let live = live(None, &[]).with_interrupt(token.clone());
        assert!(!live.snapshot().interrupted);
        token.cancel();
        assert!(live.snapshot().interrupted);
        assert!(!self::live(None, &[]).snapshot().interrupted);
    }
}
