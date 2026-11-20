//! Breakpoint mode (design D2, stress-testing "Breakpoint mode",
//! "Breakpoint refinement"): geometric steps from `--start-rps`, multiplied
//! by `--step-factor`, each lasting `--step-duration`; the first window of
//! a step is a warm-up and is not evaluated. A step breaks when SLOs fail
//! in 2 consecutive windows (and ends there). After the first broken step,
//! `--refine` bisection steps run between the last good and the first
//! broken rate.
//!
//! - A step whose windows were throttled (and nothing broke) is *rate
//!   limited*, not broken: the climb goes on, and the result records the
//!   first such rate. With `--treat-throttling-as-failure`, throttled
//!   responses are errors and can break a step.
//! - A step where the client fell behind its schedule is
//!   *client-saturated*: it never sets `breaking_rps`, and the search ends
//!   there, since going faster would only measure the client.
//! - The rate never exceeds the `max_rps` cap: a step at the cap that holds
//!   ends the search with `max_sustainable_rps` at the cap.
//! - After a broken step, the next step starts once the broken one's
//!   backlog drained: nothing is offered until a window closes with no
//!   iteration in flight (at most [`MAX_COOL_WINDOWS`] windows), and those
//!   windows belong to no step. Otherwise the late answers of the broken
//!   step would be measured in the next one.

use std::time::Duration;

use crate::error::BenchError;
use crate::stress::FailureMode;
use crate::stress::controller::{Controller, Next, Timing};
use crate::stress::endpoint::FirstFailing;
use crate::stress::window::WindowVerdict;

/// Consecutive failed windows that break a step.
pub const BREAKING_WINDOWS: u32 = 2;

/// The longest cool-down after a broken step, in windows.
pub const MAX_COOL_WINDOWS: u32 = 6;

/// The breakpoint profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BreakpointSpec {
    /// Iterations per second of the first step.
    pub start_rps: f64,
    pub step_factor: f64,
    pub step_duration: Duration,
    /// Bisection steps after the first broken one.
    pub refine: u32,
}

impl Default for BreakpointSpec {
    fn default() -> Self {
        Self {
            start_rps: 10.0,
            step_factor: 1.5,
            step_duration: Duration::from_secs(30),
            refine: 3,
        }
    }
}

/// The most bisection steps.
pub const MAX_REFINE: u32 = 20;

impl BreakpointSpec {
    pub fn validate(&self) -> Result<(), BenchError> {
        let usage = |m: &str| Err(BenchError::Usage(format!("stress: {m}")));
        if !(self.start_rps.is_finite() && self.start_rps > 0.0) {
            return usage("--start-rps must be above 0");
        }
        if !(self.step_factor.is_finite() && self.step_factor > 1.0) {
            return usage("--step-factor must be above 1");
        }
        if self.refine > MAX_REFINE {
            return usage(&format!("--refine must be at most {MAX_REFINE}"));
        }
        Ok(())
    }
}

/// How a step ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepVerdict {
    Ok,
    Broken,
    /// Throttled, but no SLO broke.
    RateLimited,
    /// The client could not keep up: no verdict on the target.
    ClientSaturated,
    /// The run stopped during the step.
    Unfinished,
}

impl StepVerdict {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Broken => "broken",
            Self::RateLimited => "rate-limited",
            Self::ClientSaturated => "client-saturated",
            Self::Unfinished => "unfinished",
        }
    }

    fn held(self) -> bool {
        matches!(self, Self::Ok | Self::RateLimited)
    }
}

/// One step of the staircase, measured over its evaluated windows.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Requests per second offered.
    pub rate: f64,
    pub verdict: StepVerdict,
    /// The failure mode of the window that broke it.
    pub failure_mode: Option<FailureMode>,
    /// Requests per second completed.
    pub achieved_rps: f64,
    /// The highest window p95, in milliseconds.
    pub p95_ms: f64,
    pub error_rate: f64,
    /// The indexes of its evaluated windows.
    pub windows: Vec<usize>,
    /// The request file that degraded first (broken steps).
    pub first_failing: Option<FirstFailing>,
}

/// Why the search ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Refinement finished after a break.
    Refined,
    /// The step at `max_rps` held.
    CapReached,
    /// The first step broke: nothing held.
    BrokeAtStart,
    /// The client could not offer the next rate.
    ClientSaturated,
    /// The run reached `max_duration`.
    MaxDuration,
    /// Ctrl+C.
    Interrupted,
    /// The kill switch stopped the load: the target was down.
    Killed,
}

/// What the breakpoint search found.
#[derive(Clone, Debug, PartialEq)]
pub struct BreakpointResult {
    pub steps: Vec<Step>,
    /// The highest rate (requests per second) that held.
    pub max_sustainable_rps: Option<f64>,
    /// The lowest rate that broke.
    pub breaking_rps: Option<f64>,
    /// How the `breaking_rps` step broke.
    pub failure_mode: Option<FailureMode>,
    /// The first rate where the target throttled without breaking.
    pub rate_limited_at: Option<f64>,
    pub stop: StopReason,
    /// The `max_rps` cap, in requests per second.
    pub max_rps: f64,
}

/// Where the search is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Climb,
    Refine { left: u32 },
}

/// The step being run.
#[derive(Clone, Debug, Default)]
struct Current {
    rate: f64,
    seen: u32,
    consecutive: u32,
    broken: bool,
    saturated: bool,
    throttled: bool,
    mode: Option<FailureMode>,
    /// The evaluated windows.
    tally: Tally,
    /// The warm-up windows, kept for a step killed before any evaluation.
    warm: Tally,
}

impl Current {
    fn at(rate: f64) -> Self {
        Self {
            rate,
            ..Self::default()
        }
    }
}

/// What a step's windows measured.
#[derive(Clone, Debug, Default)]
struct Tally {
    requests: u64,
    errors: u64,
    achieved: f64,
    p95_ms: f64,
    windows: Vec<usize>,
}

impl Tally {
    fn absorb(&mut self, v: &WindowVerdict) {
        let overall = &v.stats.overall;
        self.requests += overall.requests;
        self.errors += overall.errors.values().sum::<u64>();
        self.achieved += v.stats.completed_rps();
        self.p95_ms = self.p95_ms.max(overall.latency.p95_ms);
        self.windows.push(v.index);
    }
}

/// The breakpoint controller.
#[derive(Clone, Debug)]
pub struct Breakpoint {
    spec: BreakpointSpec,
    step_windows: u32,
    warmup: u32,
    /// The cap, in iterations per second.
    max_rate: f64,
    requests: f64,
    throttling_fails: bool,
    phase: Phase,
    current: Current,
    steps: Vec<Step>,
    good: Option<f64>,
    bad: Option<f64>,
    bad_mode: Option<FailureMode>,
    rate_limited_at: Option<f64>,
    done: Option<StopReason>,
    /// Cool-down windows closed so far, while cooling down.
    cooling: Option<u32>,
}

impl Breakpoint {
    /// A breakpoint search capped at `max_rate` iterations per second, with
    /// `requests` requests per iteration.
    pub fn new(
        spec: BreakpointSpec,
        timing: &Timing,
        max_rate: f64,
        requests: usize,
        throttling_fails: bool,
    ) -> Result<Self, BenchError> {
        spec.validate()?;
        let step_windows = timing.windows(spec.step_duration);
        let warmup = timing.warmup_windows;
        if step_windows < warmup + BREAKING_WINDOWS {
            let least = timing.window() * (warmup + BREAKING_WINDOWS);
            return Err(BenchError::Usage(format!(
                "stress: --step-duration must be at least {}s (a warm-up window and {BREAKING_WINDOWS} evaluated windows of {}s)",
                least.as_secs(),
                timing.window().as_secs()
            )));
        }
        Ok(Self {
            spec,
            step_windows,
            warmup,
            max_rate,
            requests: requests.max(1) as f64,
            throttling_fails,
            phase: Phase::Climb,
            current: Current::at(spec.start_rps.min(max_rate)),
            steps: Vec::new(),
            good: None,
            bad: None,
            bad_mode: None,
            rate_limited_at: None,
            done: None,
            cooling: None,
        })
    }

    /// Stop the search for `reason` (the run's cap or Ctrl+C), keeping the
    /// step in flight as unfinished.
    pub fn stop(&mut self, reason: StopReason) {
        if self.done.is_none() {
            self.push_step(StepVerdict::Unfinished);
            self.done = Some(reason);
        }
    }

    /// The target went down during the current step: the step broke with
    /// `mode` and the search ends. `partial` is the window cut short by
    /// the kill; a step killed during its warm-up is measured over its
    /// warm-up windows, so the row shows the failures that killed it.
    pub fn kill(&mut self, mode: FailureMode, partial: Option<&WindowVerdict>) {
        if self.done.is_some() {
            return;
        }
        let rate = self.current.rate;
        let current = &mut self.current;
        if current.tally.windows.is_empty() {
            current.tally = std::mem::take(&mut current.warm);
        }
        if let Some(window) = partial {
            current.tally.absorb(window);
        }
        current.mode = Some(mode);
        self.push_step(StepVerdict::Broken);
        if self.bad.is_none_or(|b| rate <= b) {
            self.bad = Some(rate);
            self.bad_mode = Some(mode);
        }
        self.done = Some(StopReason::Killed);
    }

    pub fn is_done(&self) -> bool {
        self.done.is_some()
    }

    /// The steps so far, newest last.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn result(&self) -> BreakpointResult {
        let rps = |rate: f64| rate * self.requests;
        BreakpointResult {
            steps: self.steps.clone(),
            max_sustainable_rps: self.good.map(rps),
            breaking_rps: self.bad.map(rps),
            failure_mode: self.bad_mode,
            rate_limited_at: self.rate_limited_at.map(rps),
            stop: self.done.unwrap_or(StopReason::Interrupted),
            max_rps: rps(self.max_rate),
        }
    }

    /// Fill in the first failing endpoint of step `index`.
    pub fn set_first_failing(&mut self, index: usize, first: Option<FirstFailing>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.first_failing = first;
        }
    }

    fn evaluate(&mut self, v: &WindowVerdict) {
        let current = &mut self.current;
        current.tally.absorb(v);
        if v.client_saturated {
            current.saturated = true;
            current.consecutive = 0;
        } else if v.failed() {
            current.consecutive += 1;
            current.mode = v.failure_mode;
            current.broken = current.consecutive >= BREAKING_WINDOWS;
        } else {
            current.consecutive = 0;
        }
        if v.throttled && !self.throttling_fails {
            current.throttled = true;
        }
    }

    fn verdict(&self) -> StepVerdict {
        let current = &self.current;
        if current.broken {
            StepVerdict::Broken
        } else if current.saturated {
            StepVerdict::ClientSaturated
        } else if current.throttled {
            StepVerdict::RateLimited
        } else {
            StepVerdict::Ok
        }
    }

    fn push_step(&mut self, verdict: StepVerdict) {
        let current = &self.current;
        let tally = &current.tally;
        let evaluated = tally.windows.len().max(1) as f64;
        self.steps.push(Step {
            rate: current.rate * self.requests,
            verdict,
            failure_mode: (verdict == StepVerdict::Broken)
                .then_some(current.mode)
                .flatten(),
            achieved_rps: tally.achieved / evaluated,
            p95_ms: tally.p95_ms,
            error_rate: ratio(tally.errors, tally.requests),
            windows: tally.windows.clone(),
            first_failing: None,
        });
    }

    /// End the current step and choose the next one (or end the search).
    fn finish_step(&mut self) {
        let verdict = self.verdict();
        let rate = self.current.rate;
        self.push_step(verdict);
        if verdict == StepVerdict::RateLimited {
            self.rate_limited_at.get_or_insert(rate);
        }
        if verdict == StepVerdict::ClientSaturated {
            self.done = Some(StopReason::ClientSaturated);
            return;
        }
        if verdict.held() {
            self.good = Some(self.good.map_or(rate, |g| g.max(rate)));
        } else {
            self.bad = Some(self.bad.map_or(rate, |b| b.min(rate)));
            if self.bad == Some(rate) {
                self.bad_mode = self.current.mode;
            }
        }
        let next = match self.phase {
            Phase::Climb => self.next_climb(verdict.held(), rate),
            Phase::Refine { left } => self.next_refine(left),
        };
        if let Some(rate) = next {
            self.current = Current::at(rate);
            if verdict == StepVerdict::Broken {
                self.cooling = Some(0);
            }
        }
    }

    /// A window of the cool-down closed: it ends once nothing is in flight.
    fn cool(&mut self, verdict: &WindowVerdict, cooled: u32) {
        let cooled = cooled + 1;
        let drained = verdict.stats.in_flight == 0;
        self.cooling = (!drained && cooled < MAX_COOL_WINDOWS).then_some(cooled);
    }

    fn next_climb(&mut self, held: bool, rate: f64) -> Option<f64> {
        if held {
            if rate >= self.max_rate * (1.0 - 1e-9) {
                self.done = Some(StopReason::CapReached);
                return None;
            }
            return Some((rate * self.spec.step_factor).min(self.max_rate));
        }
        if self.good.is_none() {
            self.done = Some(StopReason::BrokeAtStart);
            return None;
        }
        self.next_refine(self.spec.refine + 1)
    }

    /// The next bisection rate with `left` refinement steps still to run
    /// (counting the one being chosen).
    fn next_refine(&mut self, left: u32) -> Option<f64> {
        let left = left.saturating_sub(1);
        let (Some(good), Some(bad)) = (self.good, self.bad) else {
            self.done = Some(StopReason::Refined);
            return None;
        };
        if left == 0 {
            self.done = Some(StopReason::Refined);
            return None;
        }
        self.phase = Phase::Refine { left };
        Some(f64::midpoint(good, bad))
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

impl Controller for Breakpoint {
    fn rate_at(&mut self, _tick: u64) -> Next {
        if self.done.is_some() {
            Next::Done
        } else if self.cooling.is_some() {
            Next::Rate(0.0)
        } else {
            Next::Rate(self.current.rate)
        }
    }

    fn window_closed(&mut self, verdict: &WindowVerdict) {
        if self.done.is_some() {
            return;
        }
        if let Some(cooled) = self.cooling {
            self.cool(verdict, cooled);
            return;
        }
        self.current.seen += 1;
        if self.current.seen > self.warmup {
            self.evaluate(verdict);
        } else {
            self.current.warm.absorb(verdict);
        }
        if self.current.broken || self.current.seen >= self.step_windows {
            self.finish_step();
        }
    }
}

#[cfg(test)]
mod tests {
    use rustman_report::bench::StatsSummary;

    use super::*;
    use crate::stress::slo::SloFailure;
    use crate::stress::window::WindowStats;
    use crate::threshold::{Metric, Stat};

    /// A window at `rate`: failed, throttled, client-saturated or fine.
    #[derive(Clone, Copy)]
    enum W {
        Good,
        Bad,
        Throttled,
        Saturated,
    }

    fn verdict(index: usize, rate: f64, kind: W) -> WindowVerdict {
        let failed = matches!(kind, W::Bad);
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
                vec![SloFailure {
                    expression: "p95<1s".into(),
                    metric: Metric::Latency(Stat::P95),
                    actual: "2000ms".into(),
                    file: None,
                }]
            } else {
                Vec::new()
            },
            failure_mode: failed.then_some(FailureMode::Latency),
            throttled: matches!(kind, W::Throttled),
            client_saturated: matches!(kind, W::Saturated),
        }
    }

    /// Drive `bp` with a fake clock until it is done (or `limit` ticks),
    /// judging each window with `judge(rate, index_in_step)`. Returns the
    /// rate of every tick.
    fn drive(bp: &mut Breakpoint, timing: &Timing, judge: impl Fn(f64, u32) -> W) -> Vec<f64> {
        let mut rates: Vec<f64> = Vec::new();
        let mut window = 0;
        let mut in_step = 0;
        for tick in 0..10_000u64 {
            if tick > 0 && tick % u64::from(timing.window_ticks) == 0 {
                let rate = *rates.last().unwrap();
                bp.window_closed(&verdict(window, rate, judge(rate, in_step)));
                window += 1;
                in_step += 1;
            }
            match bp.rate_at(tick) {
                Next::Done => return rates,
                Next::Rate(rate) => {
                    if rates.last() != Some(&rate) {
                        in_step = 0;
                    }
                    rates.push(rate);
                }
            }
        }
        panic!("the breakpoint never ended");
    }

    /// The distinct rates of a tick schedule, in order, without the
    /// cool-downs.
    fn staircase(rates: &[f64]) -> Vec<f64> {
        let mut steps: Vec<f64> = Vec::new();
        for rate in rates.iter().filter(|r| **r > 0.0) {
            if steps.last() != Some(rate) {
                steps.push(*rate);
            }
        }
        steps
    }

    fn holds_up_to(limit: f64) -> impl Fn(f64, u32) -> W {
        move |rate, _| if rate <= limit { W::Good } else { W::Bad }
    }

    fn rounded(rates: &[f64]) -> Vec<f64> {
        rates.iter().map(|r| (r * 100.0).round() / 100.0).collect()
    }

    /// The spec's kill scenario: the target goes down during a step, which
    /// breaks with the kill's failure mode and ends the search.
    #[test]
    fn a_kill_breaks_the_current_step_and_ends_the_search() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 2, false).unwrap();
        let rates = drive(&mut bp, &timing, holds_up_to(1000.0));
        assert!(rates.len() > 10);
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 2, false).unwrap();
        bp.window_closed(&verdict(0, 10.0, W::Good));
        bp.kill(FailureMode::Connection, None);
        assert_eq!(bp.rate_at(5), Next::Done);
        let result = bp.result();
        assert_eq!(result.stop, StopReason::Killed);
        assert_eq!(result.breaking_rps, Some(20.0));
        assert_eq!(result.max_sustainable_rps, None);
        assert_eq!(result.failure_mode, Some(FailureMode::Connection));
        assert_eq!(result.steps[0].verdict, StepVerdict::Broken);
        assert_eq!(result.steps[0].failure_mode, Some(FailureMode::Connection));
        bp.kill(FailureMode::Timeouts, None);
        assert_eq!(bp.result().steps.len(), 1);
    }

    /// A kill during a step's warm-up, or before its first window closes,
    /// still measures the step: the warm-up and the cut-short window.
    #[test]
    fn a_kill_during_the_warm_up_keeps_the_step_measured() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        let mut refused = verdict(0, 10.0, W::Bad);
        refused.stats.overall.errors.insert("connection".into(), 50);
        bp.window_closed(&refused);
        let mut partial = verdict(1, 10.0, W::Bad);
        partial.stats.overall.errors.insert("connection".into(), 50);
        bp.kill(FailureMode::Connection, Some(&partial));
        let step = &bp.result().steps[0];
        assert_eq!(step.windows, [0, 1]);
        assert_eq!(step.error_rate, 1.0);
        assert!(step.achieved_rps > 0.0);

        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        bp.kill(FailureMode::Connection, Some(&partial));
        assert_eq!(bp.result().steps[0].windows, [1]);
        assert_eq!(bp.result().steps[0].error_rate, 1.0);
    }

    /// The spec's staircase: defaults, a target that holds up to 60 req/s.
    #[test]
    fn the_staircase_follows_the_step_factor_then_bisects() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        let rates = drive(&mut bp, &timing, holds_up_to(60.0));
        let steps = staircase(&rates);
        assert_eq!(
            rounded(&steps[..6]),
            [10.0, 15.0, 22.5, 33.75, 50.63, 75.94]
        );
        // 30 s per held step (6 windows of 5 s, one per second each).
        assert_eq!(rates.iter().filter(|r| **r == 10.0).count(), 30);
        // The broken step ends after its warm-up and 2 failed windows.
        let at_75 = rates
            .iter()
            .filter(|r| (**r - 75.9375).abs() < 1e-9)
            .count();
        assert_eq!(at_75, 15);
        // Bisection: 63.28 (breaks), 56.95 (holds), 60.12 (breaks).
        assert_eq!(rounded(&steps[6..]), [63.28, 56.95, 60.12]);
        let result = bp.result();
        assert_eq!(result.stop, StopReason::Refined);
        let (good, bad) = (
            result.max_sustainable_rps.unwrap(),
            result.breaking_rps.unwrap(),
        );
        assert!((good - 56.953125).abs() < 1e-6, "{good}");
        assert!((bad - 60.1171875).abs() < 1e-6, "{bad}");
        assert!((bad - good) / good < 0.1);
        assert_eq!(result.failure_mode, Some(FailureMode::Latency));
        let verdicts: Vec<StepVerdict> = result.steps.iter().map(|s| s.verdict).collect();
        assert_eq!(verdicts[5], StepVerdict::Broken);
        assert_eq!(verdicts[4], StepVerdict::Ok);
    }

    #[test]
    fn one_bad_window_does_not_break_a_step() {
        let timing = Timing::default();
        let spec = BreakpointSpec {
            refine: 0,
            ..BreakpointSpec::default()
        };
        let mut bp = Breakpoint::new(spec, &timing, 500.0, 1, false).unwrap();
        // At 10 req/s, every other window fails; at 15, all fail.
        let judge = |rate: f64, in_step: u32| match (rate > 10.0, in_step % 2) {
            (true, _) => W::Bad,
            (false, 1) => W::Bad,
            _ => W::Good,
        };
        drive(&mut bp, &timing, judge);
        let result = bp.result();
        assert_eq!(result.steps[0].verdict, StepVerdict::Ok);
        assert_eq!(result.steps[1].verdict, StepVerdict::Broken);
        assert_eq!(result.max_sustainable_rps, Some(10.0));
        assert_eq!(result.breaking_rps, Some(15.0));
    }

    #[test]
    fn the_warm_up_window_is_not_evaluated() {
        let timing = Timing::default();
        let spec = BreakpointSpec {
            refine: 0,
            ..BreakpointSpec::default()
        };
        let mut bp = Breakpoint::new(spec, &timing, 500.0, 1, false).unwrap();
        // The first window of each step fails, and only it.
        let judge = |rate: f64, in_step: u32| {
            if rate > 20.0 || in_step == 0 {
                W::Bad
            } else {
                W::Good
            }
        };
        drive(&mut bp, &timing, judge);
        let held: Vec<f64> = bp
            .result()
            .steps
            .iter()
            .filter(|s| s.verdict == StepVerdict::Ok)
            .map(|s| s.rate)
            .collect();
        assert_eq!(held, [10.0, 15.0]);
    }

    #[test]
    fn a_target_holding_every_rate_stops_at_the_cap() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        let rates = drive(&mut bp, &timing, |_, _| W::Good);
        let steps = staircase(&rates);
        assert_eq!(steps.last(), Some(&500.0));
        assert!(steps.iter().all(|r| *r <= 500.0));
        let result = bp.result();
        assert_eq!(result.stop, StopReason::CapReached);
        assert_eq!(result.max_sustainable_rps, Some(500.0));
        assert_eq!(result.breaking_rps, None);
    }

    #[test]
    fn rates_count_requests_per_iteration() {
        let timing = Timing::default();
        let spec = BreakpointSpec {
            refine: 0,
            ..BreakpointSpec::default()
        };
        // Two requests per iteration: the cap of 500 req/s is 250 it/s.
        let mut bp = Breakpoint::new(spec, &timing, 250.0, 2, false).unwrap();
        drive(&mut bp, &timing, holds_up_to(12.0));
        let result = bp.result();
        assert_eq!(result.max_sustainable_rps, Some(20.0));
        assert_eq!(result.breaking_rps, Some(30.0));
        assert_eq!(result.max_rps, 500.0);
    }

    #[test]
    fn throttling_is_rate_limited_not_broken() {
        let timing = Timing::default();
        let spec = BreakpointSpec {
            refine: 0,
            ..BreakpointSpec::default()
        };
        let mut bp = Breakpoint::new(spec, &timing, 500.0, 1, false).unwrap();
        let judge = |rate: f64, _| match rate {
            r if r > 40.0 => W::Bad,
            r if r > 20.0 => W::Throttled,
            _ => W::Good,
        };
        drive(&mut bp, &timing, judge);
        let result = bp.result();
        assert_eq!(result.rate_limited_at, Some(22.5));
        assert_eq!(result.max_sustainable_rps, Some(33.75));
        assert_eq!(result.breaking_rps, Some(50.625));
    }

    #[test]
    fn a_client_saturated_step_never_breaks_and_ends_the_search() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        let judge = |rate: f64, _| if rate > 20.0 { W::Saturated } else { W::Good };
        drive(&mut bp, &timing, judge);
        let result = bp.result();
        assert_eq!(result.stop, StopReason::ClientSaturated);
        assert_eq!(result.breaking_rps, None);
        assert_eq!(result.max_sustainable_rps, Some(15.0));
        assert_eq!(
            result.steps.last().unwrap().verdict,
            StepVerdict::ClientSaturated
        );
    }

    #[test]
    fn a_first_step_that_breaks_ends_the_search() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        drive(&mut bp, &timing, |_, _| W::Bad);
        let result = bp.result();
        assert_eq!(result.stop, StopReason::BrokeAtStart);
        assert_eq!(result.max_sustainable_rps, None);
        assert_eq!(result.breaking_rps, Some(10.0));
    }

    #[test]
    fn stopping_keeps_the_unfinished_step() {
        let timing = Timing::default();
        let mut bp = Breakpoint::new(BreakpointSpec::default(), &timing, 500.0, 1, false).unwrap();
        assert_eq!(bp.rate_at(0), Next::Rate(10.0));
        bp.stop(StopReason::MaxDuration);
        assert_eq!(bp.rate_at(1), Next::Done);
        let result = bp.result();
        assert_eq!(result.stop, StopReason::MaxDuration);
        assert_eq!(result.steps[0].verdict, StepVerdict::Unfinished);
    }

    #[test]
    fn a_step_shorter_than_its_windows_is_refused() {
        let spec = BreakpointSpec {
            step_duration: Duration::from_secs(10),
            ..BreakpointSpec::default()
        };
        let err = Breakpoint::new(spec, &Timing::default(), 500.0, 1, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "stress: --step-duration must be at least 15s (a warm-up window and 2 evaluated windows of 5s)"
        );
        for bad in [
            BreakpointSpec {
                start_rps: 0.0,
                ..BreakpointSpec::default()
            },
            BreakpointSpec {
                step_factor: 1.0,
                ..BreakpointSpec::default()
            },
            BreakpointSpec {
                refine: 99,
                ..BreakpointSpec::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    /// A broken step's backlog drains before the next step: nothing is
    /// offered until a window closes with nothing in flight, and those
    /// windows count in no step.
    #[test]
    fn the_next_step_waits_for_the_broken_steps_backlog() {
        let timing = Timing::default();
        let spec = BreakpointSpec {
            start_rps: 10.0,
            step_factor: 2.0,
            step_duration: Duration::from_secs(15),
            refine: 1,
        };
        let mut bp = Breakpoint::new(spec, &timing, 500.0, 1, false).unwrap();
        for index in 0..3 {
            bp.window_closed(&verdict(index, 10.0, W::Good));
        }
        assert_eq!(bp.rate_at(15), Next::Rate(20.0));
        for index in 3..6 {
            bp.window_closed(&verdict(index, 20.0, W::Bad));
        }
        assert_eq!(bp.steps().len(), 2);
        assert_eq!(bp.rate_at(30), Next::Rate(0.0));
        let mut backlog = verdict(5, 0.0, W::Bad);
        backlog.stats.in_flight = 7;
        bp.window_closed(&backlog);
        assert_eq!(bp.rate_at(35), Next::Rate(0.0));
        bp.window_closed(&verdict(6, 0.0, W::Good));
        assert_eq!(bp.rate_at(40), Next::Rate(15.0));
        assert_eq!(bp.steps().len(), 2, "cool-down windows are no step");
        for index in 7..10 {
            bp.window_closed(&verdict(index, 15.0, W::Good));
        }
        let steps = bp.result().steps;
        assert_eq!(steps[2].windows, [8, 9]);

        // A backlog that never drains ends the cool-down anyway.
        let mut bp = Breakpoint::new(spec, &timing, 500.0, 1, false).unwrap();
        for index in 0..3 {
            bp.window_closed(&verdict(index, 10.0, W::Good));
        }
        for index in 3..6 {
            bp.window_closed(&verdict(index, 20.0, W::Bad));
        }
        backlog.stats.in_flight = 1;
        for _ in 0..MAX_COOL_WINDOWS {
            assert_eq!(bp.rate_at(0), Next::Rate(0.0));
            bp.window_closed(&backlog);
        }
        assert_eq!(bp.rate_at(0), Next::Rate(15.0));
    }
}
