//! Preparing and running a bench: the caps, the safety policy and the
//! static request check first (nothing is sent before they pass), then the
//! VUs under one global [`Governor`] built from the `[bench]` caps.
//!
//! While load runs, a monitor closes each second of the metrics, checks
//! the abort-on-fail thresholds (after a warm-up) and a probe measures the
//! generator's own lag. A run stops new iterations at the end of its
//! profile, on Ctrl+C ([`BenchInput::interrupt`]) or on an abort-on-fail
//! breach, then gives the iterations in flight a drain period before it
//! cancels them ([`BenchInput::cancel`] cancels them at once).

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use rustman_core::model::Variables;
use rustman_core::run::{Plan, now_rfc3339};
use rustman_core::safety::{EnvironmentInfo, Governor, Limits, SafetyPolicy};
use rustman_core::secrets::provider::SecretProviders;
use rustman_core::snapshot::SnapshotMode;
use rustman_http::GovernedSender;
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_report::bench::{Aborted, BenchSecond, ErrorSample, ThresholdResult};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::caps;
use crate::closed::{Closed, ramp};
use crate::data::{DataFeed, Feeder};
use crate::error::BenchError;
use crate::lag::LagProbe;
use crate::live::Live;
use crate::metrics::{BenchSummary, LatencyKind, Metrics};
use crate::open::Open;
use crate::profile::{GRACEFUL_STOP, Limit, Profile};
use crate::safety::{BenchGate, Preflight, Safety, policy, preflight};
use crate::threshold::{NOT_MEASURED, Threshold, check_scopes, evaluate_all};
use crate::vu::{Shared, Vu};

/// How long abort-on-fail thresholds wait before their first check.
pub const ABORT_WARMUP: Duration = Duration::from_secs(10);
/// How often the metrics close a second and abort thresholds are checked.
pub const TICK: Duration = Duration::from_secs(1);
/// How long iterations in flight may finish after Ctrl+C.
pub const INTERRUPT_DRAIN: Duration = Duration::from_secs(10);
/// How long iterations in flight may finish after an abort-on-fail breach,
/// so load stops within 2 seconds of the check.
pub const ABORT_DRAIN: Duration = Duration::from_secs(1);

/// Everything a bench needs.
pub struct BenchInput<'a> {
    /// The workspace, settings, environment and secrets. VUs never write
    /// to the workspace: cookies live for the run and captures are not
    /// persisted.
    pub ctx: RunContext<'a>,
    /// The requests of one iteration, in collection order.
    pub plan: &'a Plan<'a>,
    pub profile: Profile,
    pub safety: Safety<'a>,
    /// `--var k=v`.
    pub overrides: Variables,
    /// `--timeout`: replaces every request's total timeout.
    pub timeout: Option<Duration>,
    /// `--insecure`.
    pub insecure: bool,
    pub data: Option<DataFeed>,
    /// Checked at the end (and every second for abort-on-fail ones).
    pub thresholds: Vec<Threshold>,
    /// Cancelled on a first Ctrl+C: no new iteration starts, and those in
    /// flight get [`INTERRUPT_DRAIN`] to finish.
    pub interrupt: CancellationToken,
    /// Cancelled on a second Ctrl+C: requests in flight are cancelled.
    pub cancel: CancellationToken,
    /// Resolve `{{secret:...}}` references (shared by every VU, so a
    /// value is fetched once per run).
    pub secret_providers: Option<Arc<SecretProviders>>,
    /// The workspace's trusted plugins (auth schemes, generators,
    /// transformers, assertions); `None` when it declares none.
    pub plugins: Option<std::sync::Arc<dyn rustman_core::plugins::Extensions>>,
}

/// What a bench produced.
#[derive(Clone, Debug)]
pub struct BenchOutcome {
    pub summary: BenchSummary,
    /// One point per second.
    pub series: Vec<BenchSecond>,
    /// Every threshold against the final summary, in order.
    pub thresholds: Vec<ThresholdResult>,
    /// At most 20 per error class, redacted.
    pub error_samples: Vec<ErrorSample>,
    /// `429` responses the governor backed off on (halving its rate once
    /// per rate, and pausing for `Retry-After`).
    pub backoffs: u64,
    /// Why the run stopped early, if it did.
    pub aborted: Option<Aborted>,
    /// The generator's p99 scheduling lag.
    pub generator_lag: Duration,
    /// Non-fatal problems: a generator-bound run, a lowered rate.
    pub warnings: Vec<String>,
    /// RFC 3339.
    pub started_at: String,
    pub finished_at: String,
}

impl BenchOutcome {
    /// Whether every threshold passed and none aborted the run.
    pub fn passed(&self) -> bool {
        self.aborted != Some(Aborted::Threshold) && self.thresholds.iter().all(|t| t.passed)
    }
}

/// A bench whose checks passed: running it generates load.
pub struct Bench<'a> {
    input: BenchInput<'a>,
    policy: SafetyPolicy,
    metrics: Arc<Metrics>,
    run_time: Duration,
    warmup: Duration,
}

impl<'a> Bench<'a> {
    /// Check the profile against the caps, build the policy and check every
    /// request statically. Nothing is sent.
    pub fn prepare(input: BenchInput<'a>) -> Result<Self, BenchError> {
        input.profile.validate()?;
        let settings = input.ctx.settings;
        caps::check(&input.profile, &settings.bench, input.plan.requests.len())?;
        let environment = input
            .ctx
            .environment
            .map(EnvironmentInfo::from)
            .unwrap_or_default();
        let policy = policy(&settings.safety, input.safety, environment)?;
        let with = Preflight {
            ctx: input.ctx,
            overrides: &input.overrides,
            data: input.data.as_ref().map(|d| &d.run),
        };
        preflight(input.plan, &policy, &with)?;
        let kind = match input.profile {
            Profile::ConstantArrivalRate(_) => LatencyKind::Corrected,
            _ => LatencyKind::Service,
        };
        let files: Vec<String> = input
            .plan
            .requests
            .iter()
            .map(|r| r.entry.path.to_string_lossy().replace('\\', "/"))
            .collect();
        check_scopes(&input.thresholds, &files)?;
        let run_time = caps::run_time(&input.profile, &settings.bench);
        Ok(Self {
            input,
            policy,
            metrics: Arc::new(Metrics::new(kind, files)),
            run_time,
            warmup: ABORT_WARMUP,
        })
    }

    /// Wait `warmup` (default [`ABORT_WARMUP`]) before the first check of
    /// the abort-on-fail thresholds.
    #[must_use]
    pub fn with_abort_warmup(mut self, warmup: Duration) -> Self {
        self.warmup = warmup;
        self
    }

    /// The metrics, live while the bench runs.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// A live view of the metrics with the bench's thresholds, for the
    /// dashboards. Take it before [`Self::run`].
    pub fn live(&self) -> Live {
        let planned = self.input.profile.duration().map(|d| d.min(self.run_time));
        Live::new(self.metrics(), self.input.thresholds.clone(), planned)
            .with_interrupt(self.input.interrupt.clone())
    }

    /// How long load is generated at most (before the graceful stop).
    pub fn run_time(&self) -> Duration {
        self.run_time
    }

    /// Generate the load and return what was measured.
    pub async fn run(self) -> Result<BenchOutcome, BenchError> {
        let started_at = now_rfc3339();
        let input = &self.input;
        let pool = input.profile.max_vus();
        let sender = GovernedSender::new(self.policy.clone(), self.governor(pool));
        let gates: Vec<BenchGate> = (0..pool)
            .map(|_| BenchGate::new(sender.with_policy(self.policy.clone())))
            .collect();
        let stop = input.interrupt.child_token();
        let hard = input.cancel.child_token();
        let breached = CancellationToken::new();
        let vus = self.vus(&gates, &hard)?;
        let feeder = input.data.as_ref().map(Feeder::for_feed);
        let shared = Shared {
            plan: input.plan,
            metrics: &self.metrics,
            feeder: feeder.as_ref(),
            stop: &stop,
        };
        let lag = LagProbe::new();
        let stops = Stops {
            stop: &stop,
            hard: &hard,
            breached: &breached,
        };
        self.metrics.start();
        tokio::select! {
            () = self.execute(shared, &vus) => {}
            () = self.control(&stops) => {}
            () = self.monitor(&breached) => {}
            () = lag.run() => {}
        }
        self.metrics.finish();
        let summary = self.metrics.summary();
        let backoffs: u64 = gates.iter().map(BenchGate::backoffs).sum();
        let held: u64 = gates.iter().map(BenchGate::held).sum();
        let aborted = if breached.is_cancelled() {
            Some(Aborted::Threshold)
        } else if input.interrupt.is_cancelled() || input.cancel.is_cancelled() {
            Some(Aborted::Interrupted)
        } else {
            None
        };
        let thresholds = evaluate_all(&input.thresholds, &summary);
        let mut warnings = warnings(&lag, backoffs, self.rate_capped(held, &summary));
        warnings.extend(unmeasured(&thresholds));
        Ok(BenchOutcome {
            thresholds,
            series: self.metrics.series(),
            error_samples: self.metrics.error_samples(),
            summary,
            backoffs,
            aborted,
            generator_lag: lag.p99(),
            warnings,
            started_at,
            finished_at: now_rfc3339(),
        })
    }

    /// Stop new iterations at the end of the profile, on Ctrl+C or on an
    /// abort-on-fail breach, then cancel those still in flight after the
    /// drain period of that reason. Never resolves.
    async fn control(&self, stops: &Stops<'_>) {
        let drain = tokio::select! {
            biased;
            () = self.input.interrupt.cancelled() => INTERRUPT_DRAIN,
            () = stops.breached.cancelled() => ABORT_DRAIN,
            () = tokio::time::sleep(self.run_time) => self.input.profile.graceful_stop(),
            // The executor stopped by itself (data ran out).
            () = stops.stop.cancelled() => self.input.profile.graceful_stop(),
        };
        stops.stop.cancel();
        tokio::time::sleep(drain).await;
        stops.hard.cancel();
        std::future::pending::<()>().await;
    }

    /// Close a second of the metrics every [`TICK`], and after the warm-up
    /// cancel `breached` when an abort-on-fail threshold is breached.
    /// Never resolves.
    async fn monitor(&self, breached: &CancellationToken) {
        let aborting: Vec<Threshold> = self
            .input
            .thresholds
            .iter()
            .filter(|t| t.abort)
            .cloned()
            .collect();
        let start = tokio::time::Instant::now();
        let mut tick = tokio::time::interval_at(start + TICK, TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.metrics.tick();
            if aborting.is_empty() || breached.is_cancelled() || start.elapsed() < self.warmup {
                continue;
            }
            let summary = self.metrics.summary();
            if aborting.iter().any(|t| !t.evaluate(&summary).passed) {
                breached.cancel();
            }
        }
    }

    /// The warning when `max_rps`, not the target, set the throughput: the
    /// time requests waited for it is in no latency.
    fn rate_capped(&self, held: u64, summary: &BenchSummary) -> Option<String> {
        let max_rps = self.input.ctx.settings.bench.max_rps;
        let near = summary.rps >= f64::from(max_rps.get()) * 0.6;
        (near && capped_by_rate(held, summary.overall.requests)).then(|| {
            format!(
                "{held} of {} requests waited for the max_rps limit ({max_rps}/s): throughput was capped by it, not by the target; raise [bench] max_rps in rustman.toml to go beyond",
                summary.overall.requests
            )
        })
    }

    /// The global ceiling of every executor: `max_rps` across all VUs.
    fn governor(&self, pool: u32) -> Governor {
        let caps = &self.input.ctx.settings.bench;
        Governor::new(Limits {
            rate: caps.max_rps,
            concurrency: NonZeroU32::new(pool).unwrap_or(NonZeroU32::MIN),
            max_probes: u64::MAX,
            max_duration: self.run_time + GRACEFUL_STOP * 2,
        })
    }

    /// One runner per VU, each sending through its own gate.
    fn vus<'g>(
        &self,
        gates: &'g [BenchGate],
        hard: &CancellationToken,
    ) -> Result<Vec<Vu<'g>>, BenchError>
    where
        'a: 'g,
    {
        let input = &self.input;
        let ctx = RunContext {
            in_workspace: false,
            ..input.ctx
        };
        gates
            .iter()
            .enumerate()
            .map(|(index, gate)| {
                let options = RunOptions {
                    overrides: input.overrides.clone(),
                    timeout: input.timeout,
                    insecure: input.insecure,
                    data: input.data.as_ref().map(|d| d.run.clone()),
                    cancel: hard.clone(),
                    snapshots: SnapshotMode::Ci,
                    gate: Some(gate),
                    plugins: input.plugins.clone(),
                    ..RunOptions::default()
                };
                let mut runner = Runner::new(ctx, options)
                    .map_err(|e| BenchError::Usage(format!("bench: {e}")))?;
                if let Some(providers) = &input.secret_providers {
                    runner = runner.with_secret_providers(Arc::clone(providers));
                }
                Ok(Vu { index, runner })
            })
            .collect()
    }

    async fn execute(&self, shared: Shared<'_>, vus: &[Vu<'_>]) {
        match &self.input.profile {
            Profile::ConstantVus(p) => {
                let remaining = match p.limit {
                    Limit::Iterations(n) => Some(AtomicU64::new(n)),
                    Limit::Duration(_) => None,
                };
                let closed = Closed {
                    shared,
                    think: p.think,
                    remaining,
                    grace: GRACEFUL_STOP,
                };
                closed.run(vus, None).await;
            }
            Profile::RampingVus(p) => {
                let (target, watched) = watch::channel(0);
                let closed = Closed {
                    shared,
                    think: p.think,
                    remaining: None,
                    grace: p.graceful_stop,
                };
                tokio::join!(closed.run(vus, Some(&watched)), ramp(&p.stages, &target));
            }
            Profile::ConstantArrivalRate(p) => {
                let open = Open {
                    shared,
                    rate: p.rate,
                    duration: p.duration,
                };
                open.run(vus).await;
            }
        }
    }
}

/// The tokens that stop a run.
struct Stops<'a> {
    /// No new iteration starts.
    stop: &'a CancellationToken,
    /// Iterations in flight are cancelled.
    hard: &'a CancellationToken,
    /// An abort-on-fail threshold was breached.
    breached: &'a CancellationToken,
}

/// The warnings of a run: generator-bound, rate lowered by `429`s, rate
/// capped by `max_rps`.
fn warnings(lag: &LagProbe, backoffs: u64, capped: Option<String>) -> Vec<String> {
    let mut warnings: Vec<String> = lag.warning().into_iter().collect();
    if backoffs > 0 {
        warnings.push(format!(
            "the target answered 429 Too Many Requests {backoffs} time(s); the request rate was lowered"
        ));
    }
    warnings.extend(capped);
    warnings
}

/// A threshold with nothing to measure (`checks` without assertions)
/// passes as `n/a`; say so, as it was likely written by mistake.
fn unmeasured(thresholds: &[ThresholdResult]) -> impl Iterator<Item = String> + '_ {
    thresholds
        .iter()
        .filter(|t| t.actual == NOT_MEASURED)
        .map(|t| {
            format!(
                "threshold '{}' had nothing to measure and passed as n/a",
                t.expression
            )
        })
}

/// Whether `held` requests waiting for the rate limit (at a rate near it)
/// make it, rather than the target, what set the throughput: more than one
/// in ten did.
fn capped_by_rate(held: u64, requests: u64) -> bool {
    held > 0 && held.saturating_mul(10) > requests
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_caps_the_run_when_one_request_in_ten_waits_for_it() {
        assert!(!capped_by_rate(0, 0));
        assert!(!capped_by_rate(10, 100));
        assert!(capped_by_rate(11, 100));
        assert!(capped_by_rate(5, 5));
    }

    #[test]
    fn a_threshold_with_nothing_to_measure_is_warned_about() {
        let result = |expression: &str, actual: &str| ThresholdResult {
            expression: expression.into(),
            passed: true,
            actual: actual.into(),
            abort: false,
        };
        let results = [
            result("checks>99%", NOT_MEASURED),
            result("p95<1s", "3.00ms"),
        ];
        let warnings: Vec<String> = unmeasured(&results).collect();
        assert_eq!(
            warnings,
            ["threshold 'checks>99%' had nothing to measure and passed as n/a"]
        );
    }
}
