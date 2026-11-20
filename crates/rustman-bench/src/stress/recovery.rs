//! The recovery monitor (design D7, stress-testing "Recovery monitor"):
//! after every mode, and after a kill, the health request runs once per
//! second. The target has **recovered** after 10 consecutive successes
//! (a 2xx or 3xx answer) whose latency is within 20% of the pre-stress
//! baseline; a 20 ms floor keeps the jitter of a fast local target from
//! counting against it. Without recovery within `--recovery-timeout`
//! (default 2 m) the run records the `high` finding `no-recovery`, with
//! the last observed status and latency.
//!
//! The monitor only sees a [`HealthCheck`], so tests drive it with
//! scripted probes.

use std::time::{Duration, Instant};

use futures_util::future::LocalBoxFuture;
use rustman_report::exploration::Severity;
use tokio_util::sync::CancellationToken;

use crate::metrics::ErrorClass;
use crate::stress::StressFinding;

/// The rule id of a target that did not recover.
pub const NO_RECOVERY: &str = "no-recovery";

/// The default `--recovery-timeout`.
pub const DEFAULT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(120);

/// The clocks of the health request; the defaults are the spec's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecoveryTuning {
    /// Between health requests while recovering (1 s).
    pub interval: Duration,
    /// Consecutive good answers that mean recovered (10).
    pub successes: u32,
    /// How far above the baseline a good answer's latency may be (20%).
    pub tolerance: f64,
    /// The least latency slack, whatever the baseline (20 ms).
    pub floor: Duration,
    /// Health requests sent back to back before load, for the baseline.
    pub baseline_probes: u32,
    /// Between health requests while load runs, for the kill switch (1 s).
    pub during_load: Duration,
    /// How long error shares must stay high for the kill switch (5 s).
    pub kill_window: Duration,
    /// Between minimization replays (500 ms: 2 req/s).
    pub replay_interval: Duration,
}

impl Default for RecoveryTuning {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            successes: 10,
            tolerance: 0.2,
            floor: Duration::from_millis(20),
            baseline_probes: 3,
            during_load: Duration::from_secs(1),
            kill_window: crate::stress::kill::KILL_WINDOW,
            replay_interval: Duration::from_millis(500),
        }
    }
}

/// One health request and how it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The status, when an answer came.
    pub status: Option<u16>,
    pub latency: Duration,
    /// Why it failed: an error class, `None` for a 2xx or 3xx answer.
    pub error: Option<ErrorClass>,
    /// What happened instead of an answer (redacted).
    pub detail: Option<String>,
}

impl Probe {
    /// A 2xx or 3xx answer.
    pub fn healthy(&self) -> bool {
        self.status.is_some_and(|s| (200..400).contains(&s))
    }

    /// No answer at all (refused, reset, timed out): what the kill switch
    /// counts.
    pub fn unanswered(&self) -> Option<ErrorClass> {
        match self.status {
            Some(_) => None,
            None => Some(self.error.unwrap_or(ErrorClass::Connect)),
        }
    }

    /// `503 in 12ms`, or `no answer (connect: refused) after 3ms`.
    pub fn describe(&self) -> String {
        let latency = self.latency.as_millis();
        match (self.status, &self.detail) {
            (Some(status), _) => format!("{status} in {latency}ms"),
            (None, Some(detail)) => format!("no answer ({detail}) after {latency}ms"),
            (None, None) => format!("no answer after {latency}ms"),
        }
    }
}

/// Sends the health request.
pub trait HealthCheck {
    fn probe(&self) -> LocalBoxFuture<'_, Probe>;
}

/// How recovery ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStatus {
    Recovered,
    /// `--recovery-timeout` elapsed first: `no-recovery`.
    NotRecovered,
    /// Not checked: no health request, or it already failed before load.
    Skipped,
    /// A second Ctrl+C stopped the check.
    Cancelled,
}

impl RecoveryStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Recovered => "recovered",
            Self::NotRecovered => "not-recovered",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One health request of the recovery timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbePoint {
    /// Since the monitor started (when load stopped).
    pub offset: Duration,
    pub probe: Probe,
    /// It counted toward recovery.
    pub good: bool,
}

/// What the monitor found.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryResult {
    pub status: RecoveryStatus,
    /// From the end of load to the first answer of the good streak.
    pub after: Option<Duration>,
    /// The health request's pre-stress latency.
    pub baseline: Option<Duration>,
    /// The health request file.
    pub health: Option<String>,
    pub timeline: Vec<ProbePoint>,
    /// Why recovery was skipped.
    pub reason: Option<String>,
}

impl RecoveryResult {
    /// Not checked, for `reason`.
    pub fn skipped(health: Option<String>, reason: &str) -> Self {
        Self {
            status: RecoveryStatus::Skipped,
            after: None,
            baseline: None,
            health,
            timeline: Vec::new(),
            reason: Some(reason.to_owned()),
        }
    }

    /// The last health request sent.
    pub fn last(&self) -> Option<&Probe> {
        self.timeline.last().map(|p| &p.probe)
    }

    /// `no-recovery`, when the target did not recover in `timeout`.
    pub fn finding(&self, timeout: Duration) -> Option<StressFinding> {
        if self.status != RecoveryStatus::NotRecovered {
            return None;
        }
        let file = self.health.as_deref().unwrap_or("the health request");
        let last = match self.last() {
            Some(probe) if probe.status.is_some() => format!("answered {}", probe.describe()),
            Some(probe) => format!("got {}", probe.describe()),
            None => "was never sent".to_owned(),
        };
        let message = format!(
            "{file} had not recovered {} after load stopped: the last health request {last}",
            span(timeout)
        );
        Some(StressFinding::new(NO_RECOVERY, Severity::High, message))
    }
}

/// `2m`, `90s` or `300ms`.
pub fn span(d: Duration) -> String {
    let secs = d.as_secs();
    if d.subsec_millis() != 0 || secs == 0 {
        format!("{}ms", d.as_millis())
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// The baseline latency of `probes` sent before load: the median of the
/// healthy ones; `None` when none was healthy.
pub fn baseline(probes: &[Probe]) -> Option<Duration> {
    let mut healthy: Vec<Duration> = probes
        .iter()
        .filter(|p| p.healthy())
        .map(|p| p.latency)
        .collect();
    healthy.sort();
    healthy.get(healthy.len() / 2).copied()
}

/// Whether `probe` counts toward recovery against `baseline`.
fn good(probe: &Probe, baseline: Option<Duration>, tuning: &RecoveryTuning) -> bool {
    if !probe.healthy() {
        return false;
    }
    baseline.is_none_or(|b| {
        let slack = b.mul_f64(tuning.tolerance).max(tuning.floor);
        probe.latency <= b + slack
    })
}

/// Send the health request every `tuning.interval` until the target has
/// recovered, `timeout` elapsed or `cancel` fired.
pub async fn monitor(
    check: &dyn HealthCheck,
    baseline: Option<Duration>,
    timeout: Duration,
    tuning: &RecoveryTuning,
    cancel: &CancellationToken,
) -> RecoveryResult {
    let start = Instant::now();
    let mut result = RecoveryResult {
        status: RecoveryStatus::NotRecovered,
        after: None,
        baseline,
        health: None,
        timeline: Vec::new(),
        reason: None,
    };
    let mut streak: Option<Duration> = None;
    let mut good_ones = 0;
    let mut next = tokio::time::Instant::now();
    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return result;
        }
        let probe = tokio::select! {
            () = cancel.cancelled() => {
                result.status = RecoveryStatus::Cancelled;
                return result;
            }
            probe = tokio::time::timeout(remaining, check.probe()) => match probe {
                Ok(probe) => probe,
                Err(_) => return result,
            },
        };
        let offset = start.elapsed();
        let counts = good(&probe, baseline, tuning);
        if counts {
            good_ones += 1;
            streak.get_or_insert(offset.saturating_sub(probe.latency));
        } else {
            good_ones = 0;
            streak = None;
        }
        result.timeline.push(ProbePoint {
            offset,
            probe,
            good: counts,
        });
        if good_ones >= tuning.successes {
            result.status = RecoveryStatus::Recovered;
            result.after = streak;
            return result;
        }
        next += tuning.interval;
        tokio::select! {
            () = cancel.cancelled() => {
                result.status = RecoveryStatus::Cancelled;
                return result;
            }
            () = tokio::time::sleep_until(next) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Answers `status` with `latency` from the `from`-th request on,
    /// `503` before.
    struct Scripted {
        from: u32,
        status: u16,
        latency: Duration,
        sent: AtomicU32,
    }

    impl HealthCheck for Scripted {
        fn probe(&self) -> LocalBoxFuture<'_, Probe> {
            let n = self.sent.fetch_add(1, Ordering::SeqCst);
            let status = if n >= self.from { self.status } else { 503 };
            Box::pin(async move {
                Probe {
                    status: Some(status),
                    latency: self.latency,
                    error: (status >= 500).then_some(ErrorClass::Status5xx),
                    detail: None,
                }
            })
        }
    }

    fn scripted(from: u32, status: u16, latency_ms: u64) -> Scripted {
        Scripted {
            from,
            status,
            latency: Duration::from_millis(latency_ms),
            sent: AtomicU32::new(0),
        }
    }

    fn fast() -> RecoveryTuning {
        RecoveryTuning {
            interval: Duration::from_millis(10),
            ..RecoveryTuning::default()
        }
    }

    fn probe(status: Option<u16>, ms: u64) -> Probe {
        Probe {
            status,
            latency: Duration::from_millis(ms),
            error: status.is_none().then_some(ErrorClass::Connect),
            detail: status.is_none().then(|| "connect: refused".to_owned()),
        }
    }

    #[tokio::test]
    async fn ten_good_answers_in_a_row_are_a_recovery() {
        let check = scripted(4, 200, 5);
        let baseline = Some(Duration::from_millis(5));
        let cancel = CancellationToken::new();
        let result = monitor(&check, baseline, Duration::from_secs(30), &fast(), &cancel).await;
        assert_eq!(result.status, RecoveryStatus::Recovered);
        assert_eq!(result.timeline.len(), 14);
        assert!(result.timeline[..4].iter().all(|p| !p.good));
        let first_good = result.timeline[4].offset;
        let after = result.after.unwrap();
        assert!(after <= first_good, "{after:?} {first_good:?}");
        assert_eq!(result.finding(Duration::from_secs(30)), None);
    }

    #[tokio::test]
    async fn slow_answers_do_not_count() {
        let check = scripted(0, 200, 200);
        let baseline = Some(Duration::from_millis(100));
        let cancel = CancellationToken::new();
        let timeout = Duration::from_millis(300);
        let result = monitor(&check, baseline, timeout, &fast(), &cancel).await;
        assert_eq!(result.status, RecoveryStatus::NotRecovered);
        assert!(result.timeline.iter().all(|p| !p.good));
        let finding = result.finding(Duration::from_secs(120)).unwrap();
        assert_eq!(finding.rule, NO_RECOVERY);
        assert_eq!(finding.severity, Severity::High);
        assert!(
            finding.message.ends_with("answered 200 in 200ms"),
            "{}",
            finding.message
        );
    }

    #[tokio::test]
    async fn a_target_that_never_recovers_is_reported_with_its_last_answer() {
        let check = scripted(u32::MAX, 200, 7);
        let cancel = CancellationToken::new();
        let timeout = Duration::from_millis(200);
        let result = monitor(&check, None, timeout, &fast(), &cancel).await;
        assert_eq!(result.status, RecoveryStatus::NotRecovered);
        assert_eq!(result.last().unwrap().status, Some(503));
        let message = result.finding(timeout).unwrap().message;
        assert_eq!(
            message,
            "the health request had not recovered 200ms after load stopped: the last health request answered 503 in 7ms"
        );
    }

    #[test]
    fn a_health_request_with_no_answer_reads_as_such() {
        let mut unanswered = RecoveryResult::skipped(None, "");
        unanswered.status = RecoveryStatus::NotRecovered;
        unanswered.timeline.push(ProbePoint {
            offset: Duration::ZERO,
            probe: probe(None, 1),
            good: false,
        });
        let message = unanswered.finding(Duration::from_secs(2)).unwrap().message;
        assert!(
            message.ends_with("the last health request got no answer (connect: refused) after 1ms"),
            "{message}"
        );
        assert!(!message.contains("answered no answer"), "{message}");
    }

    #[tokio::test]
    async fn cancelling_stops_the_check() {
        let check = scripted(u32::MAX, 200, 1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = monitor(&check, None, Duration::from_secs(60), &fast(), &cancel).await;
        assert_eq!(result.status, RecoveryStatus::Cancelled);
        assert_eq!(result.finding(Duration::from_secs(60)), None);
    }

    #[test]
    fn the_baseline_is_the_median_healthy_latency() {
        let probes = [
            probe(Some(200), 30),
            probe(Some(500), 1),
            probe(Some(200), 10),
            probe(None, 2),
            probe(Some(204), 20),
        ];
        assert_eq!(baseline(&probes), Some(Duration::from_millis(20)));
        assert_eq!(baseline(&probes[1..2]), None);
    }

    #[test]
    fn latency_is_judged_against_the_baseline_with_a_floor() {
        let tuning = RecoveryTuning::default();
        let base = Some(Duration::from_millis(1000));
        assert!(good(&probe(Some(200), 1200), base, &tuning));
        assert!(!good(&probe(Some(200), 1201), base, &tuning));
        let tiny = Some(Duration::from_millis(1));
        assert!(good(&probe(Some(200), 21), tiny, &tuning));
        assert!(!good(&probe(Some(500), 1), tiny, &tuning));
        assert!(good(&probe(Some(302), 999), None, &tuning));
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!(span(Duration::from_secs(120)), "2m");
        assert_eq!(span(Duration::from_secs(90)), "90s");
        assert_eq!(span(Duration::from_millis(1500)), "1500ms");
    }

    #[test]
    fn probes_describe_what_came_back() {
        assert_eq!(probe(Some(503), 12).describe(), "503 in 12ms");
        assert_eq!(
            probe(None, 3).describe(),
            "no answer (connect: refused) after 3ms"
        );
        assert_eq!(probe(None, 3).unanswered(), Some(ErrorClass::Connect));
        assert_eq!(probe(Some(503), 3).unanswered(), None);
    }
}
