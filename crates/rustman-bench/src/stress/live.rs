//! The live view of a stress run (stress-testing "Live stress dashboard"):
//! the run loop updates a [`StressLive`] once per tick and per closed
//! window, and front ends read [`StressSnapshot`]s of it (the TTY
//! dashboard, at most 4 times a second) or the step lines (one per step
//! when stdout is not a terminal), never holding the load up: an update
//! is a short lock on a small state.
//!
//! A step is a breakpoint step; in the other modes it is a stretch of
//! windows at the same rate (a spike's phases), at most a minute long (a
//! soak's minutes). Like the summary, the live view holds rates, counts,
//! timings, SLO expressions, file names and finding messages (already
//! redacted), never request content.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rustman_report::bench::StatsSummary;

use crate::stress::breakpoint::Step;
use crate::stress::kill::KillState;
use crate::stress::run::StressOutcome;
use crate::stress::window::WindowVerdict;
use crate::stress::{ModeKind, StressFinding, format_rate};

/// The most windows a snapshot shows.
pub const RECENT_WINDOWS: usize = 12;

/// The most windows in a step of a mode without steps (a minute of 5 s
/// windows).
const SEGMENT_WINDOWS: usize = 12;

/// One step line: `step 3: offered 22.5 req/s, achieved 22.4 req/s, p95 31ms, errors 0.00%: ok`.
#[derive(Clone, Debug, PartialEq)]
pub struct StepLine {
    /// 1-based.
    pub number: usize,
    pub offered: f64,
    pub achieved: f64,
    pub p95_ms: f64,
    pub error_rate: f64,
    /// `ok` or `broken` (a breakpoint step may also be `rate-limited`,
    /// `client-saturated` or `unfinished`).
    pub verdict: String,
}

impl fmt::Display for StepLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "step {}: offered {} req/s, achieved {} req/s, p95 {:.0}ms, errors {:.2}%: {}",
            self.number,
            format_rate(self.offered),
            format_rate(self.achieved),
            self.p95_ms,
            self.error_rate * 100.0,
            self.verdict
        )
    }
}

/// One closed SLO window, as the dashboard lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowLine {
    pub index: usize,
    /// Requests per second offered.
    pub offered: f64,
    pub p95_ms: f64,
    pub error_rate: f64,
    /// `ok`, `failed` or `client-saturated`.
    pub status: &'static str,
    /// The SLOs it failed, as written.
    pub failed: Vec<String>,
}

/// The run so far.
#[derive(Clone, Debug, PartialEq)]
pub struct StressSnapshot {
    pub mode: ModeKind,
    /// `health baseline`, `load`, `recovery`, `minimizing`, `wire faults`
    /// or `done`.
    pub phase: &'static str,
    pub elapsed: Duration,
    /// The step under way (1-based).
    pub step: usize,
    /// Requests per second offered now.
    pub offered: f64,
    /// Requests per second completed in the last window.
    pub achieved: f64,
    /// Valid traffic in the last window.
    pub valid: StatsSummary,
    /// Fuzzed traffic so far, when the run fuzzes.
    pub fuzzed: Option<StatsSummary>,
    /// Error classes of the last window.
    pub errors: BTreeMap<String, u64>,
    /// The last [`RECENT_WINDOWS`] windows, oldest first.
    pub windows: Vec<WindowLine>,
    pub steps: Vec<StepLine>,
    pub kill: KillState,
    /// `killed: target down at ...`, once the kill switch tripped.
    pub killed: Option<String>,
    /// `high no-recovery: ...`, as findings arrive.
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct Segment {
    offered: f64,
    windows: Vec<(f64, f64, f64, bool)>,
}

struct State {
    started: Instant,
    snapshot: StressSnapshot,
    segment: Segment,
}

/// A handle on a run's live view; cheap to clone and `Send`.
#[derive(Clone)]
pub struct StressLive(Arc<Mutex<State>>);

impl fmt::Debug for StressLive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StressLive").finish_non_exhaustive()
    }
}

impl StressLive {
    pub fn new(mode: ModeKind) -> Self {
        let snapshot = StressSnapshot {
            mode,
            phase: "starting",
            elapsed: Duration::ZERO,
            step: 1,
            offered: 0.0,
            achieved: 0.0,
            valid: StatsSummary::default(),
            fuzzed: None,
            errors: BTreeMap::new(),
            windows: Vec::new(),
            steps: Vec::new(),
            kill: KillState::default(),
            killed: None,
            findings: Vec::new(),
        };
        Self(Arc::new(Mutex::new(State {
            started: Instant::now(),
            snapshot,
            segment: Segment::default(),
        })))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The run so far.
    pub fn snapshot(&self) -> StressSnapshot {
        let state = self.state();
        let mut snapshot = state.snapshot.clone();
        snapshot.elapsed = state.started.elapsed();
        snapshot
    }

    /// The step lines from the `from`-th on (0-based).
    pub fn lines_since(&self, from: usize) -> Vec<StepLine> {
        let state = self.state();
        state.snapshot.steps.iter().skip(from).cloned().collect()
    }

    pub fn phase(&self, phase: &'static str) {
        self.state().snapshot.phase = phase;
    }

    /// The offered rate, in requests per second.
    pub fn rate(&self, offered: f64) {
        self.state().snapshot.offered = offered;
    }

    pub fn kill(&self, kill: KillState) {
        self.state().snapshot.kill = kill;
    }

    /// A window closed; `fuzzed` is fuzzed traffic so far.
    pub fn window(&self, verdict: &WindowVerdict, fuzzed: Option<StatsSummary>) {
        let line = window_line(verdict);
        let mut state = self.state();
        let snapshot = &mut state.snapshot;
        snapshot.achieved = verdict.stats.completed_rps();
        snapshot.valid = verdict.stats.overall.clone();
        snapshot.errors = verdict.stats.overall.errors.clone();
        snapshot.fuzzed = fuzzed;
        snapshot.windows.push(line.clone());
        let extra = snapshot.windows.len().saturating_sub(RECENT_WINDOWS);
        snapshot.windows.drain(..extra);
        if snapshot.mode != ModeKind::Breakpoint {
            state.segment_window(&line, verdict);
        }
    }

    /// The breakpoint's steps so far: lines for the new ones.
    pub fn steps(&self, steps: &[Step]) {
        let mut state = self.state();
        let snapshot = &mut state.snapshot;
        for step in steps.iter().skip(snapshot.steps.len()) {
            let number = snapshot.steps.len() + 1;
            snapshot.steps.push(StepLine {
                number,
                offered: step.rate,
                achieved: step.achieved_rps,
                p95_ms: step.p95_ms,
                error_rate: step.error_rate,
                verdict: step.verdict.name().to_owned(),
            });
        }
        snapshot.step = snapshot.steps.len() + 1;
    }

    /// The run is over: the last step, the kill and the findings.
    pub fn finished(&self, outcome: &StressOutcome) {
        if let Some(bp) = &outcome.breakpoint {
            self.steps(&bp.steps);
        }
        let mut state = self.state();
        state.close_segment();
        let snapshot = &mut state.snapshot;
        snapshot.phase = "done";
        snapshot.killed = outcome.killed.map(|k| k.to_string());
        snapshot.findings = outcome.findings.iter().map(finding_line).collect();
    }
}

impl State {
    fn segment_window(&mut self, line: &WindowLine, verdict: &WindowVerdict) {
        let same = (self.segment.offered - line.offered).abs() < 1e-9;
        if !self.segment.windows.is_empty()
            && (!same || self.segment.windows.len() >= SEGMENT_WINDOWS)
        {
            self.close_segment();
        }
        self.segment.offered = line.offered;
        let failed = verdict.failed() && !verdict.client_saturated;
        self.segment.windows.push((
            verdict.stats.completed_rps(),
            line.p95_ms,
            line.error_rate,
            failed,
        ));
    }

    fn close_segment(&mut self) {
        let segment = std::mem::take(&mut self.segment);
        if segment.windows.is_empty() {
            return;
        }
        let n = segment.windows.len() as f64;
        let broken = segment.windows.windows(2).any(|w| w[0].3 && w[1].3);
        let snapshot = &mut self.snapshot;
        let number = snapshot.steps.len() + 1;
        snapshot.steps.push(StepLine {
            number,
            offered: segment.offered,
            achieved: segment.windows.iter().map(|w| w.0).sum::<f64>() / n,
            p95_ms: segment.windows.iter().map(|w| w.1).fold(0.0, f64::max),
            error_rate: segment.windows.iter().map(|w| w.2).sum::<f64>() / n,
            verdict: if broken { "broken" } else { "ok" }.to_owned(),
        });
        snapshot.step = snapshot.steps.len() + 1;
    }
}

fn window_line(verdict: &WindowVerdict) -> WindowLine {
    let overall = &verdict.stats.overall;
    let status = if verdict.client_saturated {
        "client-saturated"
    } else if verdict.failed() {
        "failed"
    } else {
        "ok"
    };
    WindowLine {
        index: verdict.index,
        offered: verdict.rate * verdict.stats.requests_per_iteration.max(1) as f64,
        p95_ms: overall.latency.p95_ms,
        error_rate: overall.error_rate,
        status,
        failed: verdict
            .failures
            .iter()
            .map(|f| f.expression.clone())
            .collect(),
    }
}

/// `high no-recovery: <message>`.
pub fn finding_line(finding: &StressFinding) -> String {
    format!("{} {}: {}", finding.severity, finding.rule, finding.message)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustman_report::exploration::Severity;

    use super::*;
    use crate::stress::window::WindowStats;

    fn verdict(index: usize, rate: f64, failed: bool) -> WindowVerdict {
        WindowVerdict {
            index,
            rate,
            stats: WindowStats {
                length: Duration::from_secs(5),
                scheduled: (rate * 5.0) as u64,
                requests_per_iteration: 1,
                overall: StatsSummary {
                    requests: (rate * 5.0) as u64,
                    ..StatsSummary::default()
                },
                files: Vec::new(),
                throttled: 0,
                throttled_5xx: 0,
                fuzzed: 0,
                in_flight: 0,
            },
            failures: if failed {
                vec![crate::stress::slo::SloFailure {
                    expression: "p95<1s".into(),
                    metric: crate::threshold::Metric::Latency(crate::threshold::Stat::P95),
                    actual: "2000ms".into(),
                    file: None,
                }]
            } else {
                Vec::new()
            },
            failure_mode: None,
            throttled: false,
            client_saturated: false,
        }
    }

    #[test]
    fn a_spike_has_one_step_per_phase() {
        let live = StressLive::new(ModeKind::Spike);
        let mut index = 0;
        for (rate, failed, windows) in [(10.0, false, 2), (100.0, true, 3), (10.0, false, 2)] {
            for _ in 0..windows {
                live.window(&verdict(index, rate, failed), None);
                index += 1;
            }
        }
        assert_eq!(live.lines_since(0).len(), 2);
        let line = live.lines_since(1)[0].to_string();
        assert_eq!(
            line,
            "step 2: offered 100 req/s, achieved 100 req/s, p95 0ms, errors 0.00%: broken"
        );
        let snapshot = live.snapshot();
        assert_eq!(snapshot.windows.len(), 7);
        assert_eq!(snapshot.windows[2].failed, ["p95<1s"]);
        assert_eq!(snapshot.windows[2].status, "failed");
        assert_eq!(snapshot.step, 3);
    }

    #[test]
    fn a_long_flat_run_has_a_step_per_minute() {
        let live = StressLive::new(ModeKind::Soak);
        for index in 0..30 {
            live.window(&verdict(index, 5.0, false), None);
        }
        assert_eq!(live.lines_since(0).len(), 2);
        assert_eq!(live.snapshot().windows.len(), RECENT_WINDOWS);
    }

    #[test]
    fn findings_are_listed_by_severity_and_rule() {
        let finding = StressFinding::new("no-recovery", Severity::High, "still 503".into());
        assert_eq!(finding_line(&finding), "high no-recovery: still 503");
    }
}
