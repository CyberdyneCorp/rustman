//! Preparing and running a stress test: the shared safety policy, the
//! stress gate, the `[stress]` caps and the static request check first
//! (nothing is sent before they pass), then the mode's controller over the
//! arrival-rate scheduler, with every request sent through a
//! [`GovernedSender`] governed by the `[stress]` caps.
//!
//! Each VU is a task owning what it needs (the workspace, secrets and
//! senders are shared through `Arc`s) and building its own [`Runner`]
//! inside the task, so a VU borrows nothing from the caller; VU tasks run
//! on the worker threads of [`crate::stress::workers`], spread over the
//! cores, while the controller and the scheduler run on the caller's
//! runtime.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustman_core::model::{RequestSettings, Variables};
use rustman_core::run::{Plan, RequestStatus, now_rfc3339, plan};
use rustman_core::safety::{EnvironmentInfo, Governor, Limits, Permission, SafetyPolicy};
use rustman_core::snapshot::SnapshotMode;
use rustman_core::stress::StressSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest};
use rustman_core::{Environment, SecretStore, Workspace};
use rustman_explore::ApiModel;
use rustman_explore::mutate::schema::SchemaMutator;
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
use rustman_http::{GovernedSender, SendOptions};
use rustman_report::bench::{BenchSecond, LatencyKind, StatsSummary};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::error::BenchError;
use crate::metrics::{BenchSummary, ErrorClass, Metrics, Sample, describe};
use crate::safety::{BenchGate, Preflight, Safety, outside_proxy, policy, preflight};
use crate::stress::after::{self, Before};
use crate::stress::breakpoint::{Breakpoint, BreakpointResult, StepVerdict, StopReason};
use crate::stress::classify::classify;
use crate::stress::controller::{Baseline, Controller, Flat, Next, Timing};
use crate::stress::endpoint::first_failing;
use crate::stress::executor::{Job, LagMeter, Pool, Scheduler};
use crate::stress::fuzz::findings::{FuzzFindings, Observed};
use crate::stress::fuzz::gate::{BombCell, Fuzzed, StressGate};
use crate::stress::fuzz::impact::{self, Valid};
use crate::stress::fuzz::{DEFAULT_RATIO, FuzzPlan, random_seed};
use crate::stress::gate::{self, Ask};
use crate::stress::kill::{DownReason, KillSwitch, Killed, Pulse};
use crate::stress::live::StressLive;
use crate::stress::malformed::{Caps, Family, PayloadCache};
use crate::stress::probe::{Health, Prober, ReplayGate, file_name, first_get};
use crate::stress::recovery::{
    DEFAULT_RECOVERY_TIMEOUT, RecoveryResult, RecoveryStatus, RecoveryTuning,
};
use crate::stress::saturation::SaturationGuard;
use crate::stress::slo::{Slo, Slos};
use crate::stress::soak::{self, SoakClock, SoakResult, SoakSpec};
use crate::stress::spike::{self, Spike, SpikeResult};
use crate::stress::window::{Recorder, THROTTLED_SHARE, WindowVerdict, throttled};
use crate::stress::wire::{self, WireConsent, WireResult, WireTuning};
use crate::stress::workers::{MakeVu, Workers, worker_count};
use crate::stress::{ModeKind, ModeSpec, StressFinding, caps, fd};
use crate::vu::{corrected, service_time};

/// How long iterations in flight may finish once load stops.
pub const DRAIN: Duration = Duration::from_secs(5);

/// Time single requests may take beyond the run and the recovery check
/// (the drain, then at most 50 replays per finding at 2 req/s).
const PROBE_GRACE: Duration = Duration::from_secs(3600);

/// The clean phase before fuzzing, when `--baseline` does not set it.
pub const DEFAULT_FUZZ_BASELINE: Duration = Duration::from_secs(30);

/// The clocks of a run; the defaults are the spec's, tests shorten them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tuning {
    pub timing: Timing,
    pub soak: SoakClock,
    /// Pauses the scheduler after each dispatch, to simulate a client that
    /// cannot keep up. Zero outside tests.
    #[doc(hidden)]
    pub dispatch_delay: Duration,
    /// The clocks of wire faults.
    pub wire: WireTuning,
    /// The clocks of the health request, the kill switch and replays.
    pub recovery: RecoveryTuning,
}

/// Everything a stress run needs. The workspace and the secret store are
/// shared with the VU tasks.
pub struct StressInput {
    pub workspace: Arc<Workspace>,
    /// The active environment's name.
    pub environment: Option<String>,
    pub secrets: Arc<SecretStore>,
    /// The request files and folders (empty: the whole workspace).
    pub scopes: Vec<PathBuf>,
    pub mode: ModeSpec,
    /// `--target`: the only origin requests may go to.
    pub target: String,
    pub permission: Permission,
    /// `--production <host>`.
    pub production: Option<String>,
    /// `--fuzz-ratio`.
    pub fuzz_ratio: Option<f64>,
    /// `--malformed`.
    pub malformed: Vec<String>,
    /// `--wire`.
    pub wire: Vec<String>,
    /// `--allow-wire-faults`.
    pub allow_wire_faults: bool,
    /// `--seed`: the seed of fuzzed slots and variants (default: random).
    pub seed: Option<u64>,
    /// `--spec`: the document whose schemas fuzzed variants break.
    pub spec: Option<ApiModel>,
    /// `--baseline` of a run with fuzzing: its clean phase (default
    /// [`DEFAULT_FUZZ_BASELINE`]).
    pub fuzz_baseline: Option<Duration>,
    /// `--slo` (after `[stress.slo]`).
    pub slos: Vec<Slo>,
    /// `--var k=v`.
    pub overrides: Variables,
    /// `--timeout`.
    pub timeout: Option<Duration>,
    /// `--insecure`.
    pub insecure: bool,
    /// `--treat-throttling-as-failure`.
    pub treat_throttling_as_failure: bool,
    /// `--max-vus`: the VU pool (default: the `max_rps` cap), at most the
    /// `max_vus` cap.
    pub max_vus: Option<u32>,
    /// `--health`: the health request file (default: the plan's first GET).
    pub health: Option<PathBuf>,
    /// `--recovery-timeout` (default 2 m).
    pub recovery_timeout: Option<Duration>,
    /// Cancelled on a first Ctrl+C: load stops.
    pub interrupt: CancellationToken,
    /// Cancelled on a second Ctrl+C: requests in flight are cancelled.
    pub cancel: CancellationToken,
    pub tuning: Tuning,
}

/// What a stress run produced.
#[derive(Clone, Debug)]
pub struct StressOutcome {
    pub mode: ModeKind,
    /// The seed of fuzzed slots and variants.
    pub seed: u64,
    pub slos: Vec<String>,
    pub summary: BenchSummary,
    /// One point per tick.
    pub series: Vec<BenchSecond>,
    pub windows: Vec<WindowVerdict>,
    pub breakpoint: Option<BreakpointResult>,
    pub spike: Option<SpikeResult>,
    pub soak: Option<SoakResult>,
    /// Fuzz mixing, when the run fuzzed.
    pub fuzz: Option<FuzzOutcome>,
    /// What each wire-fault family observed.
    pub wire: Vec<WireResult>,
    pub findings: Vec<StressFinding>,
    /// The kill switch stopped the load.
    pub killed: Option<Killed>,
    /// The recovery check after the load.
    pub recovery: RecoveryResult,
    /// Some window was client-saturated.
    pub client_saturated: bool,
    /// Ctrl+C stopped the run.
    pub interrupted: bool,
    /// The run reached `max_duration` (or `max_soak_duration`).
    pub reached_max_duration: bool,
    pub warnings: Vec<String>,
    /// When load started (the clock of the series and windows).
    pub load_started: Instant,
    pub started_at: String,
    pub finished_at: String,
}

/// Fuzz mixing as it ran: fuzzed traffic, measured apart from valid
/// traffic, and valid traffic before and while fuzzing.
#[derive(Clone, Debug)]
pub struct FuzzOutcome {
    /// The share of requests replaced.
    pub ratio: f64,
    /// The fuzzed exchanges.
    pub fuzzed: BenchSummary,
    /// The clean baseline phase.
    pub baseline_ticks: u64,
    /// Valid traffic during the baseline.
    pub baseline: Option<Valid>,
    /// Valid traffic while fuzzing, at the baseline's rate.
    pub valid: Option<Valid>,
}

/// A token cancelled as soon as `a` or `b` is; it stops watching `b`
/// when dropped.
fn either(a: &CancellationToken, b: &CancellationToken) -> EitherToken {
    let token = a.child_token();
    let watched = token.clone();
    let other = b.clone();
    let watcher = tokio::spawn(async move {
        tokio::select! {
            () = other.cancelled() => watched.cancel(),
            () = watched.cancelled() => {}
        }
    });
    EitherToken { token, watcher }
}

/// The token of [`either`], with the task that forwards the second token.
struct EitherToken {
    token: CancellationToken,
    watcher: tokio::task::JoinHandle<()>,
}

impl std::ops::Deref for EitherToken {
    type Target = CancellationToken;

    fn deref(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for EitherToken {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl FuzzOutcome {
    /// Whether at least one request was replaced by a fuzzed variant.
    pub fn sent_any(&self) -> bool {
        self.fuzzed.overall.requests > 0
    }

    /// `fuzz-degrades-valid-traffic` when valid traffic degraded while
    /// fuzzing. Without a fuzzed request nothing can be blamed on fuzzing
    /// (a plan without a JSON body to mutate): drift is the target's own.
    pub fn impact(&self) -> Option<StressFinding> {
        let (Some(baseline), Some(valid)) = (&self.baseline, &self.valid) else {
            return None;
        };
        if !self.sent_any() {
            return None;
        }
        impact::judge(baseline, valid)
    }
}

/// A stress run whose checks passed: running it generates load.
pub struct Stress {
    input: StressInput,
    seed: u64,
    fuzz: Option<Arc<FuzzPlan>>,
    baseline: Option<Baseline>,
    wire: Option<(WireConsent, SendOptions)>,
    fuzzed: Arc<Metrics>,
    findings: Arc<FuzzFindings>,
    policy: SafetyPolicy,
    slos: Slos,
    files: Vec<String>,
    requests: usize,
    max_rate: f64,
    run_time: Duration,
    recovery_timeout: Duration,
    metrics: Arc<Metrics>,
    live: StressLive,
    warnings: Vec<String>,
}

/// How the control loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Done,
    Interrupted,
    MaxDuration,
    /// The kill switch tripped.
    Killed(DownReason),
}

/// The controller of a mode.
enum Mode {
    Breakpoint(Box<Breakpoint>),
    Spike(Spike),
    Flat(Flat),
}

impl Mode {
    fn controller(&mut self) -> &mut dyn Controller {
        match self {
            Self::Breakpoint(c) => c.as_mut(),
            Self::Spike(c) => c,
            Self::Flat(c) => c,
        }
    }
}

impl Stress {
    /// Check the target, the stress rules, the caps and every request.
    /// Nothing is sent.
    pub fn prepare(input: StressInput) -> Result<Self, BenchError> {
        let ws = &*input.workspace;
        let stress = &ws.settings.stress;
        let environment = environment(ws, input.environment.as_deref());
        let plan = plan(ws, &input.scopes, &[]);
        if plan.is_empty() {
            return Err(usage("no requests to run (check the paths)"));
        }
        let safety = Safety {
            target: &input.target,
            permission: input.permission,
            production: input.production.as_deref(),
        };
        let env_info = environment.map(EnvironmentInfo::from).unwrap_or_default();
        let policy = policy(&ws.settings.safety, safety, env_info).map_err(restyle)?;
        let ask = Ask {
            fuzz_ratio: input.fuzz_ratio,
            malformed: &input.malformed,
            wire: &input.wire,
            allow_wire_faults: input.allow_wire_faults,
        };
        let consent = gate::wire_consent(stress, &policy, &ask)?;
        caps::check(&input.mode, stress, plan.len())?;
        caps::pool(input.max_vus, stress)?;
        let with = Preflight {
            ctx: context(ws, environment, &input.secrets),
            overrides: &input.overrides,
            data: None,
        };
        preflight(&plan, &policy, &with).map_err(restyle)?;
        if let Some(health) = &input.health {
            let health_plan = plan_of(ws, health)?;
            preflight(&health_plan, &policy, &with).map_err(restyle)?;
        }
        let files = file_names(&plan);
        let slos = Slos::new(
            &stress.slo_expressions(),
            &input.slos,
            input.mode.kind(),
            &files,
        )?;
        let seed = input.seed.unwrap_or_else(random_seed);
        let fuzz = fuzz_plan(&input, stress, seed)?.map(Arc::new);
        let baseline = fuzz.as_ref().map(|_| {
            let duration = input.fuzz_baseline.unwrap_or(DEFAULT_FUZZ_BASELINE);
            Baseline::new(input.mode.initial_rate(), duration, &input.tuning.timing)
        });
        let wire = match consent {
            Some(consent) => {
                let options = wire_options(ws, &policy, input.insecure)?;
                Some((consent, options))
            }
            None => None,
        };
        let requests = plan.len();
        let max_rate = caps::max_rate(stress, requests);
        // Building the controller checks the step duration against the
        // windows.
        controller(
            &input.mode,
            &input.tuning,
            max_rate,
            requests,
            input.treat_throttling_as_failure,
        )?;
        let warnings = fd::fd_warning(fd::fd_limit(), stress.max_rps.get())
            .into_iter()
            .collect();
        let live = StressLive::new(input.mode.kind());
        Ok(Self {
            run_time: caps::run_time(&input.mode, stress),
            recovery_timeout: input.recovery_timeout.unwrap_or(DEFAULT_RECOVERY_TIMEOUT),
            live,
            metrics: Arc::new(Metrics::new(LatencyKind::Corrected, files.clone())),
            fuzzed: Arc::new(Metrics::new(LatencyKind::Corrected, files.clone())),
            findings: Arc::new(FuzzFindings::new(stress.expected_max_body)),
            seed,
            fuzz,
            baseline,
            wire,
            policy,
            slos,
            files,
            requests,
            max_rate,
            warnings,
            input,
        })
    }

    /// The pre-flight warnings (a low open-file limit), to show before
    /// load starts.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The seed of fuzzed slots and variants, to print before load starts.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The SLOs in force, as written.
    pub fn slos(&self) -> Vec<String> {
        self.slos.list().iter().map(ToString::to_string).collect()
    }

    /// The metrics, live while the run goes on.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// The live view of the run, for dashboards and progress lines.
    pub fn live(&self) -> StressLive {
        self.live.clone()
    }

    /// Generate the load and return what was measured. Fails only when
    /// the worker threads cannot be started.
    pub async fn run(self) -> Result<StressOutcome, BenchError> {
        let started_at = now_rfc3339();
        let input = &self.input;
        let probe_gate = ReplayGate::new(BenchGate::new(GovernedSender::new(
            self.policy.clone(),
            self.probe_governor(),
        )));
        let ws = &*input.workspace;
        let environment = environment(ws, input.environment.as_deref());
        let health_plan = match &input.health {
            Some(path) => Some(plan_of(ws, path)?),
            None => None,
        };
        let options = RunOptions {
            overrides: input.overrides.clone(),
            timeout: input.timeout,
            insecure: input.insecure,
            cancel: input.cancel.clone(),
            snapshots: SnapshotMode::Ci,
            gate: Some(&probe_gate),
            ..RunOptions::default()
        };
        let runner = Runner::new(context(ws, environment, &input.secrets), options)
            .map_err(|e| usage(&format!("cannot prepare the health request: {e}")))?;
        let main_plan = plan(ws, &input.scopes, &[]);
        let health = match &health_plan {
            Some(p) => p
                .requests
                .first()
                .map_or(Health::None, |r| Health::File(r.clone())),
            None => first_get(&main_plan).map_or(Health::None, Health::InPlan),
        };
        let prober = Prober::new(runner, &probe_gate, main_plan, health);
        self.live.phase("health baseline");
        let before = after::before_load(&prober, &input.tuning.recovery).await;
        let mut mode = controller(
            &input.mode,
            &input.tuning,
            self.max_rate,
            self.requests,
            input.treat_throttling_as_failure,
        )
        .expect("checked by prepare");
        let recorder = Arc::new(Recorder::new(
            self.files.len(),
            input.treat_throttling_as_failure,
        ));
        let stop = input.interrupt.child_token();
        let hard = input.cancel.child_token();
        let pulse = Arc::new(Pulse::default());
        let shared = Arc::new(self.vu_context(recorder.clone(), pulse.clone(), hard.clone()));
        let make: Arc<MakeVu> =
            Arc::new(move |index, jobs, done| Box::pin(vu(shared.clone(), index, jobs, done)));
        let workers = Workers::start(worker_count(), make)
            .map_err(|e| usage(&format!("cannot start the load threads: {e}")))?;
        let spawn = |index, jobs, done| workers.spawn(index, jobs, done);
        let pool_size = caps::pool(input.max_vus, &input.workspace.settings.stress)
            .expect("checked by prepare");
        let mut pool = Pool::new(&spawn, usize::try_from(pool_size).unwrap_or(usize::MAX));
        let lag = LagMeter::default();
        let fuzzing = AtomicBool::new(false);
        let scheduler = Scheduler {
            metrics: &self.metrics,
            recorder: &recorder,
            lag: &lag,
            dispatch_delay: input.tuning.dispatch_delay,
            fuzzing: self.fuzz.as_ref().map(|_| &fuzzing),
        };
        let (rate_tx, rate_rx) = watch::channel(0.0);
        let mut run = Control::new(&self, &recorder, &lag, &fuzzing, &pulse);
        let (health_tx, mut health_rx) = mpsc::unbounded_channel();
        let load_started = Instant::now();
        self.live.phase("load");
        self.metrics.start();
        self.fuzzed.start();
        let control = async {
            let ended = run.control(&mut mode, &rate_tx, &mut health_rx).await;
            if matches!(ended, Ended::Killed(_)) {
                // The target is down: requests in flight are cancelled too.
                hard.cancel();
            }
            stop.cancel();
            ended
        };
        let probing =
            health_during_load(&prober, &before, &input.tuning.recovery, &stop, health_tx);
        let (ended, (), ()) =
            tokio::join!(control, scheduler.run(&mut pool, rate_rx, &stop), probing);
        pool.close();
        workers.finish(DRAIN, &hard).await;
        self.metrics.finish();
        self.fuzzed.finish();
        let mut outcome = run.outcome(mode, ended, load_started, started_at, String::new());
        self.live.phase("recovery");
        outcome.recovery = after::recover(
            &prober,
            &before,
            self.recovery_timeout,
            &input.tuning.recovery,
            &input.cancel,
        )
        .await;
        self.after_recovery(&prober, &mut outcome, ended).await;
        outcome.finished_at = now_rfc3339();
        self.live.finished(&outcome);
        Ok(outcome)
    }

    /// Minimization, wire faults and the recovery finding, once the
    /// recovery check is over: neither replays nor wire faults are sent
    /// after Ctrl+C or to a target that has not recovered.
    async fn after_recovery(&self, prober: &Prober<'_>, outcome: &mut StressOutcome, ended: Ended) {
        let input = &self.input;
        let status = outcome.recovery.status;
        let fit = matches!(status, RecoveryStatus::Recovered | RecoveryStatus::Skipped);
        if let Some(finding) = outcome.recovery.finding(self.recovery_timeout) {
            outcome.findings.push(finding);
        }
        // Ctrl+C during the recovery check counts as much as during load.
        let interrupted = ended == Ended::Interrupted || input.interrupt.is_cancelled();
        if !fit || interrupted {
            if self.wire.is_some() || !self.findings.crash_inputs().is_empty() {
                let reason = if interrupted {
                    "interrupted (Ctrl+C)".to_owned()
                } else {
                    format!("recovery {}", status.name())
                };
                outcome
                    .warnings
                    .push(format!("minimization and wire faults skipped: {reason}"));
            }
            return;
        }
        self.live.phase("minimizing");
        // Replays stop on the first Ctrl+C as well as on the second.
        let stop = either(&input.interrupt, &input.cancel);
        after::minimize_all(
            prober,
            &self.files,
            self.findings.crash_inputs(),
            &mut outcome.findings,
            &input.tuning.recovery,
            &stop,
        )
        .await;
        self.live.phase("wire faults");
        let (results, findings) = self.wire_phase(ended).await;
        outcome.wire = results;
        outcome.findings.extend(findings);
    }

    /// The governor of single requests (health, replays): the `[stress]`
    /// rate cap, one at a time, for as long as the run, the recovery check
    /// and the replays may take.
    fn probe_governor(&self) -> Governor {
        let caps = &self.input.workspace.settings.stress;
        Governor::new(Limits {
            rate: caps.max_rps,
            concurrency: NonZeroU32::MIN,
            max_probes: u64::MAX,
            max_duration: self.run_time + self.recovery_timeout + PROBE_GRACE,
        })
        .without_backoff()
    }

    /// Wire faults, once load stopped (not after Ctrl+C).
    async fn wire_phase(&self, ended: Ended) -> (Vec<WireResult>, Vec<StressFinding>) {
        let Some((consent, options)) = &self.wire else {
            return (Vec::new(), Vec::new());
        };
        if ended == Ended::Interrupted {
            return (Vec::new(), Vec::new());
        }
        let stop = self.input.interrupt.child_token();
        let (results, findings) = wire::run(consent, options, &self.input.tuning.wire, &stop).await;
        if stop.is_cancelled() {
            return (results, Vec::new());
        }
        (results, findings)
    }

    fn vu_context(
        &self,
        recorder: Arc<Recorder>,
        pulse: Arc<Pulse>,
        hard: CancellationToken,
    ) -> VuContext {
        let input = &self.input;
        let caps = &input.workspace.settings.stress;
        let pool = caps::pool(input.max_vus, caps).expect("checked by prepare");
        let governor = Governor::new(Limits {
            rate: caps.max_rps,
            concurrency: NonZeroU32::new(pool).unwrap_or(NonZeroU32::MIN),
            max_probes: u64::MAX,
            max_duration: self.run_time + DRAIN * 4,
        })
        .without_backoff()
        .with_burst(caps::governor_burst(caps.max_rps));
        VuContext {
            fuzz: self.fuzz.clone(),
            bomb: BombCell::default(),
            fuzzed: self.fuzzed.clone(),
            findings: self.findings.clone(),
            files: self.files.clone(),
            workspace: input.workspace.clone(),
            environment: input.environment.clone(),
            secrets: input.secrets.clone(),
            scopes: input.scopes.clone(),
            policy: self.policy.clone(),
            sender: GovernedSender::new(self.policy.clone(), governor),
            overrides: input.overrides.clone(),
            timeout: input.timeout,
            insecure: input.insecure,
            metrics: self.metrics.clone(),
            recorder,
            pulse,
            hard,
        }
    }
}

/// The control loop and what it collects.
struct Control<'a> {
    stress: &'a Stress,
    recorder: &'a Recorder,
    lag: &'a LagMeter,
    pulse: &'a Pulse,
    kill: KillSwitch,
    /// Set when the fuzz phase starts (after the baseline).
    fuzzing: &'a AtomicBool,
    baseline: Option<Baseline>,
    guard: SaturationGuard,
    windows: Vec<WindowVerdict>,
    buckets: Vec<StatsSummary>,
    /// The lowest p95 of a good window so far.
    reference_p95: f64,
    rate: f64,
    /// When the window in progress started.
    window_started: tokio::time::Instant,
}

impl<'a> Control<'a> {
    fn new(
        stress: &'a Stress,
        recorder: &'a Recorder,
        lag: &'a LagMeter,
        fuzzing: &'a AtomicBool,
        pulse: &'a Pulse,
    ) -> Self {
        let tuning = &stress.input.tuning;
        Self {
            stress,
            recorder,
            lag,
            pulse,
            kill: KillSwitch::new(tuning.recovery.kill_window, tuning.timing.tick),
            fuzzing,
            baseline: stress.baseline,
            guard: SaturationGuard::default(),
            windows: Vec::new(),
            buckets: Vec::new(),
            reference_p95: f64::INFINITY,
            rate: 0.0,
            window_started: tokio::time::Instant::now(),
        }
    }

    fn tuning(&self) -> &Tuning {
        &self.stress.input.tuning
    }

    /// Ticks of the clean baseline phase (0 without fuzzing).
    fn baseline_ticks(&self) -> u64 {
        self.baseline.map_or(0, |b| b.ticks())
    }

    fn rate_at(&self, tick: u64, mode: &mut Mode) -> Next {
        match &self.baseline {
            Some(baseline) => baseline.rate_at(tick, mode.controller()),
            None => mode.controller().rate_at(tick),
        }
    }

    /// Set the rate every tick until the mode is done, Ctrl+C or the run
    /// time cap.
    async fn control(
        &mut self,
        mode: &mut Mode,
        rate: &watch::Sender<f64>,
        health: &mut mpsc::UnboundedReceiver<Option<ErrorClass>>,
    ) -> Ended {
        let timing = self.tuning().timing;
        let start = tokio::time::Instant::now();
        self.window_started = start;
        for tick in 0.. {
            if tick >= self.baseline_ticks() {
                self.fuzzing.store(true, Ordering::Relaxed);
            }
            match self.rate_at(tick, mode) {
                Next::Done => return Ended::Done,
                Next::Rate(r) => {
                    self.rate = r;
                    rate.send_replace(r);
                    self.stress.live.rate(r * self.stress.requests as f64);
                }
            }
            let next = start + timing.tick * u32::try_from(tick + 1).unwrap_or(u32::MAX);
            if let Some(ended) = self.wait(next, health).await {
                return self.ended(ended, mode);
            }
            self.tick(tick + 1, mode);
            let down = self.kill.tick(tick, self.pulse.take());
            self.stress.live.kill(self.kill.state());
            if let Some(reason) = down {
                return self.ended(Ended::Killed(reason), mode);
            }
            if start.elapsed() >= self.stress.run_time {
                return Ended::MaxDuration;
            }
        }
        Ended::Done
    }

    /// Wait for `until`, judging health answers as they come; `Some` when
    /// the run must end first (Ctrl+C, or the target is down).
    async fn wait(
        &mut self,
        until: tokio::time::Instant,
        health: &mut mpsc::UnboundedReceiver<Option<ErrorClass>>,
    ) -> Option<Ended> {
        let interrupt = self.stress.input.interrupt.clone();
        loop {
            tokio::select! {
                biased;
                () = interrupt.cancelled() => return Some(Ended::Interrupted),
                Some(unanswered) = health.recv() => {
                    if let Some(reason) = self.kill.health(unanswered) {
                        self.stress.live.kill(self.kill.state());
                        return Some(Ended::Killed(reason));
                    }
                }
                () = tokio::time::sleep_until(until) => return None,
            }
        }
    }

    /// A kill breaks the breakpoint's current step, measured with the
    /// window the kill cut short.
    fn ended(&mut self, ended: Ended, mode: &mut Mode) -> Ended {
        if let (Ended::Killed(reason), Mode::Breakpoint(bp)) = (ended, mode) {
            let partial = self.window_started.elapsed();
            let window = (!partial.is_zero()).then(|| self.close_window(partial));
            bp.kill(reason.failure_mode(), window.as_ref());
            self.windows.extend(window);
        }
        ended
    }

    /// Close the tick that just ended (`tick` ticks so far): the metrics'
    /// point, the lag, and the window or trend bucket it completes.
    fn tick(&mut self, tick: u64, mode: &mut Mode) {
        let tuning = *self.tuning();
        self.stress.metrics.tick();
        self.stress.fuzzed.tick();
        self.guard.tick(self.lag.take());
        if tick.is_multiple_of(u64::from(tuning.timing.window_ticks.max(1))) {
            let verdict = self.close_window(tuning.timing.window());
            match &mut self.baseline {
                Some(baseline) => baseline.window_closed(&verdict, mode.controller()),
                None => mode.controller().window_closed(&verdict),
            }
            let fuzzed = self
                .stress
                .fuzz
                .as_ref()
                .map(|_| self.stress.fuzzed.summary().overall);
            self.stress.live.window(&verdict, fuzzed);
            if let Mode::Breakpoint(bp) = mode {
                self.stress.live.steps(bp.steps());
            }
            self.windows.push(verdict);
        }
        let Some(since) = tick.checked_sub(self.baseline_ticks()) else {
            return;
        };
        if since == 0 {
            // The trend of a soak starts after the baseline.
            self.recorder.close_bucket();
        } else if matches!(self.stress.input.mode, ModeSpec::Soak(_))
            && since.is_multiple_of(tuning.timing.ticks(tuning.soak.bucket))
        {
            self.buckets.push(self.recorder.close_bucket());
        }
    }

    fn close_window(&mut self, length: Duration) -> WindowVerdict {
        let stress = self.stress;
        self.window_started = tokio::time::Instant::now();
        let mut stats = self
            .recorder
            .close_window(length, &stress.files, stress.requests);
        stats.in_flight = stress.metrics.active_vus() as u64;
        let failures = stress.slos.evaluate(&stats);
        let client_saturated = self.guard.close_window();
        let throttling_fails = stress.input.treat_throttling_as_failure;
        let reference = if self.reference_p95.is_finite() {
            self.reference_p95
        } else {
            0.0
        };
        let failure_mode = classify(&stats, &failures, reference, throttling_fails);
        if failures.is_empty() && stats.overall.requests > 0 {
            self.reference_p95 = self.reference_p95.min(stats.overall.latency.p95_ms);
        }
        WindowVerdict {
            index: self.windows.len(),
            rate: self.rate,
            throttled: stats.throttled_share() >= THROTTLED_SHARE,
            stats,
            failures,
            failure_mode,
            client_saturated,
        }
    }

    fn outcome(
        self,
        mode: Mode,
        ended: Ended,
        load_started: Instant,
        started_at: String,
        finished_at: String,
    ) -> StressOutcome {
        let stress = self.stress;
        let series = stress.metrics.series();
        let mut outcome = StressOutcome {
            mode: stress.input.mode.kind(),
            seed: stress.seed,
            fuzz: self.fuzz_outcome(),
            wire: Vec::new(),
            slos: stress.slos(),
            summary: stress.metrics.summary(),
            series,
            windows: Vec::new(),
            breakpoint: None,
            spike: None,
            soak: None,
            findings: Vec::new(),
            killed: match ended {
                Ended::Killed(reason) => Some(Killed {
                    rps: self.rate * stress.requests as f64,
                    reason,
                }),
                _ => None,
            },
            recovery: RecoveryResult::skipped(None, "not run yet"),
            client_saturated: self.guard.saw_saturation(),
            interrupted: ended == Ended::Interrupted,
            reached_max_duration: ended == Ended::MaxDuration,
            warnings: Vec::new(),
            load_started,
            started_at,
            finished_at,
        };
        match mode {
            Mode::Breakpoint(bp) => {
                outcome.breakpoint = Some(self.breakpoint_result(*bp, ended));
            }
            Mode::Spike(spike) => {
                let timing = &stress.input.tuning.timing;
                let skip = usize::try_from(self.baseline_ticks()).unwrap_or(usize::MAX);
                let series = outcome.series.get(skip..).unwrap_or_default();
                outcome.spike = Some(spike::analyze(series, &spike, timing));
            }
            Mode::Flat(_) => {
                if let ModeSpec::Soak(spec) = &stress.input.mode {
                    let result = self.soak_result(spec);
                    outcome.findings.extend(result.findings.iter().cloned());
                    outcome.soak = Some(result);
                }
            }
        }
        outcome.findings.extend(stress.findings.findings());
        if let Some(fuzz) = &outcome.fuzz {
            outcome.findings.extend(fuzz.impact());
        }
        if outcome.client_saturated {
            outcome.warnings.push(
                "the client fell more than 100 ms behind its schedule for 3 s or more: those windows are marked client-saturated and do not count as server breaks".into(),
            );
        }
        outcome.windows = self.windows;
        outcome
    }

    /// Fuzzed traffic, and valid traffic before and while fuzzing (at the
    /// baseline's rate, client-saturated windows aside).
    fn fuzz_outcome(&self) -> Option<FuzzOutcome> {
        let plan = self.stress.fuzz.as_ref()?;
        let baseline = self.baseline?;
        let split = usize::try_from(baseline.windows()).unwrap_or(usize::MAX);
        let before = self.windows.iter().take(split).map(|w| &w.stats);
        let during = self
            .windows
            .iter()
            .skip(split)
            .filter(|w| !w.client_saturated && (w.rate - baseline.rate()).abs() < 1e-9)
            .map(|w| &w.stats);
        Some(FuzzOutcome {
            ratio: plan.ratio,
            fuzzed: self.stress.fuzzed.summary(),
            baseline_ticks: baseline.ticks(),
            baseline: Valid::of(before),
            valid: Valid::of(during),
        })
    }

    fn breakpoint_result(&self, mut bp: Breakpoint, ended: Ended) -> BreakpointResult {
        match ended {
            Ended::Interrupted => bp.stop(StopReason::Interrupted),
            Ended::MaxDuration => bp.stop(StopReason::MaxDuration),
            Ended::Done | Ended::Killed(_) => {}
        }
        let broken: Vec<usize> = bp
            .steps()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.verdict == StepVerdict::Broken)
            .map(|(i, _)| i)
            .collect();
        for index in broken {
            let windows: Vec<&WindowVerdict> = bp.steps()[..=index]
                .iter()
                .flat_map(|s| s.windows.iter())
                .filter_map(|w| self.windows.get(*w))
                .collect();
            let first = first_failing(&windows, &self.stress.slos);
            bp.set_first_failing(index, first);
        }
        bp.result()
    }

    fn soak_result(&self, spec: &SoakSpec) -> SoakResult {
        soak::analyze(&self.buckets, spec, &self.stress.input.tuning.soak)
    }
}

/// The controller of `mode`.
fn controller(
    mode: &ModeSpec,
    tuning: &Tuning,
    max_rate: f64,
    requests: usize,
    throttling_fails: bool,
) -> Result<Mode, BenchError> {
    let timing = &tuning.timing;
    Ok(match mode {
        ModeSpec::Breakpoint(spec) => Mode::Breakpoint(Box::new(Breakpoint::new(
            *spec,
            timing,
            max_rate,
            requests,
            throttling_fails,
        )?)),
        ModeSpec::Spike(spec) => Mode::Spike(Spike::new(*spec, timing)),
        ModeSpec::Soak(spec) => Mode::Flat(Flat::new(spec.flat(), timing)),
        ModeSpec::Fuzz(spec) => Mode::Flat(Flat::new(*spec, timing)),
    })
}

/// What every VU task shares.
struct VuContext {
    fuzz: Option<Arc<FuzzPlan>>,
    bomb: BombCell,
    /// The fuzzed exchanges, measured apart from valid traffic.
    fuzzed: Arc<Metrics>,
    findings: Arc<FuzzFindings>,
    /// The plan's file names.
    files: Vec<String>,
    workspace: Arc<Workspace>,
    environment: Option<String>,
    secrets: Arc<SecretStore>,
    scopes: Vec<PathBuf>,
    policy: SafetyPolicy,
    /// One governor for every VU; each VU sends through a sender of its
    /// own (its own connection pool) sharing it.
    sender: GovernedSender,
    overrides: Variables,
    timeout: Option<Duration>,
    insecure: bool,
    metrics: Arc<Metrics>,
    recorder: Arc<Recorder>,
    /// What the kill switch counts.
    pulse: Arc<Pulse>,
    hard: CancellationToken,
}

/// One VU: a runner of its own, running the plan once per job until its
/// queue closes.
async fn vu(
    shared: Arc<VuContext>,
    index: usize,
    mut jobs: mpsc::Receiver<Job>,
    done: mpsc::UnboundedSender<usize>,
) {
    let ws = &*shared.workspace;
    let environment = environment(ws, shared.environment.as_deref());
    let plan = plan(ws, &shared.scopes, &[]);
    let bench = BenchGate::new(shared.sender.with_policy(shared.policy.clone()));
    let gate = StressGate::new(bench, shared.fuzz.clone(), shared.bomb.clone());
    let options = RunOptions {
        overrides: shared.overrides.clone(),
        timeout: shared.timeout,
        insecure: shared.insecure,
        cancel: shared.hard.clone(),
        snapshots: SnapshotMode::Ci,
        gate: Some(&gate),
        ..RunOptions::default()
    };
    let Ok(runner) = Runner::new(context(ws, environment, &shared.secrets), options) else {
        return;
    };
    while let Some(job) = jobs.recv().await {
        shared.metrics.vu_started();
        iteration(&runner, &plan, &shared, &gate, job).await;
        shared.metrics.vu_stopped();
        if done.send(index).is_err() {
            break;
        }
    }
}

/// Every request of the plan in order, each recorded in the bench metrics
/// and the stress windows, or, for a fuzzed slot, apart. Latency counts
/// from the scheduled start for the first request (coordinated-omission
/// correction), as in the bench.
async fn iteration(
    runner: &Runner<'_>,
    plan: &Plan<'_>,
    shared: &VuContext,
    gate: &StressGate,
    job: Job,
) {
    let started = Instant::now();
    runner.seed(&plan.requests);
    runner.forget_iteration_secrets();
    let mut mark = Some(job.scheduled);
    let requests = plan.requests.len() as u64;
    for (file, request) in plan.requests.iter().enumerate() {
        let slot = job.fuzz_iteration.map(|i| i * requests + file as u64);
        let fuzzing = shared.arm(gate, slot);
        let begun = Instant::now();
        let sent = runner.send(request, 1).await;
        let ended = Instant::now();
        let fuzzed = fuzzing.then(|| gate.take_fuzzed()).flatten();
        if sent.record.status == RequestStatus::Skipped {
            shared.metrics.iteration_interrupted();
            return;
        }
        let latency = corrected(service_time(&sent.record, ended - begun), ended, mark);
        let sample = Sample::of(file, latency, &sent.record);
        shared.pulse.record(sample.error);
        match fuzzed {
            Some(fuzzed) => shared.record_fuzzed(&fuzzed, &sample, &sent),
            None => shared.record_valid(&sample, &sent),
        }
        mark = Some(ended);
    }
    shared.metrics.iteration_done(started.elapsed());
}

impl VuContext {
    /// Arm `gate` when slot `slot` is fuzzed.
    fn arm(&self, gate: &StressGate, slot: Option<u64>) -> bool {
        let fuzzed = self.fuzz.as_deref().zip(slot).filter(|(p, s)| p.fuzzed(*s));
        if let Some((_, slot)) = fuzzed {
            gate.arm(slot);
        }
        fuzzed.is_some()
    }

    fn record_valid(&self, sample: &Sample, sent: &Sent) {
        self.metrics.record(sample);
        self.recorder.record(sample, throttled(&sent.record));
        if let Some(class) = sample.error {
            let message = sent.redactor.text(&describe(&sent.record)).into_owned();
            self.metrics.sample_error(class, sample.file, message);
        }
    }

    fn record_fuzzed(&self, fuzzed: &Fuzzed, sample: &Sample, sent: &Sent) {
        self.fuzzed.record(sample);
        self.recorder.fuzzed();
        let file = self.files.get(sample.file).map_or("", String::as_str);
        self.findings.observe(&Observed {
            fuzzed,
            file,
            record: &sent.record,
            error: sample.error,
            redactor: &sent.redactor,
        });
    }
}

/// Fuzz mixing for `input`: with `--fuzz-ratio`, the fuzz mode or
/// `--malformed`.
fn fuzz_plan(
    input: &StressInput,
    settings: &StressSettings,
    seed: u64,
) -> Result<Option<FuzzPlan>, BenchError> {
    let mutations = input.fuzz_ratio.is_some() || input.mode.kind() == ModeKind::Fuzz;
    if input.spec.is_some() && !mutations {
        return Err(usage(
            "--spec needs fuzz mixing (--fuzz-ratio or the fuzz mode)",
        ));
    }
    if !mutations && input.malformed.is_empty() {
        return Ok(None);
    }
    let families = input
        .malformed
        .iter()
        .map(|f| f.parse::<Family>().map_err(|e| usage(&e)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(FuzzPlan {
        seed,
        ratio: input.fuzz_ratio.unwrap_or(DEFAULT_RATIO),
        mutations,
        schema: input
            .spec
            .clone()
            .map(|model| Arc::new(SchemaMutator::new(model, seed))),
        families,
        caps: Caps::of(settings),
        expected_max_body: settings.expected_max_body,
        payloads: PayloadCache::default(),
    }))
}

/// The connection options of wire faults to the policy's target: the
/// workspace's TLS files and proxy, as for a request to it. A proxy
/// outside the policy is refused.
fn wire_options(
    ws: &Workspace,
    policy: &SafetyPolicy,
    insecure: bool,
) -> Result<SendOptions, BenchError> {
    let target = policy.target().to_string();
    let request = ResolvedRequest {
        method: "GET".into(),
        url: Resolved::literal(&target),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: rustman_core::vars::TlsFiles::default(),
        secrets: Vec::new(),
    };
    let mut options = SendOptions::for_request(&ws.settings, &ws.root, &request)
        .map_err(|e| usage(&format!("wire faults: {e}")))?;
    if insecure {
        options.tls.verify = false;
    }
    if let Some(proxy) = outside_proxy(policy, &options.proxy, &target) {
        return Err(BenchError::Refused(format!(
            "stress: wire faults: proxy not allowed ({})",
            proxy.host
        )));
    }
    Ok(options)
}

fn environment<'w>(ws: &'w Workspace, name: Option<&str>) -> Option<&'w Environment> {
    let name = name?;
    ws.environments.iter().find(|e| e.name == name)
}

fn context<'a>(
    ws: &'a Workspace,
    environment: Option<&'a Environment>,
    secrets: &'a SecretStore,
) -> RunContext<'a> {
    RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment,
        secrets,
        in_workspace: false,
    }
}

/// The plan of `--health <file>`: its one request.
fn plan_of<'w>(ws: &'w Workspace, path: &std::path::Path) -> Result<Plan<'w>, BenchError> {
    let found = plan(ws, &[path.to_path_buf()], &[]);
    if found.len() != 1 {
        return Err(usage(&format!(
            "--health {}: expected one request file",
            path.display()
        )));
    }
    Ok(found)
}

/// The health request once per `tuning.during_load` while load runs, its
/// unanswered ones sent to the kill switch. Only a health request that
/// answered before load is watched.
async fn health_during_load(
    prober: &Prober<'_>,
    before: &Before,
    tuning: &RecoveryTuning,
    stop: &CancellationToken,
    unanswered: mpsc::UnboundedSender<Option<ErrorClass>>,
) {
    if !before.healthy() {
        return;
    }
    let mut next = tokio::time::Instant::now() + tuning.during_load;
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep_until(next) => {}
        }
        next += tuning.during_load;
        let probe = tokio::select! {
            () = stop.cancelled() => return,
            probe = prober.health() => probe,
        };
        let Some(probe) = probe else { return };
        if unanswered.send(probe.unanswered()).is_err() {
            return;
        }
    }
}

fn file_names(plan: &Plan<'_>) -> Vec<String> {
    plan.requests.iter().map(file_name).collect()
}

fn usage(message: &str) -> BenchError {
    BenchError::Usage(format!("stress: {message}"))
}

/// The bench's checks name the command they run under: say `stress`.
fn restyle(err: BenchError) -> BenchError {
    let restyled = |m: String| match m.strip_prefix("bench: ") {
        Some(rest) => format!("stress: {rest}"),
        None => m,
    };
    match err {
        BenchError::Usage(m) => BenchError::Usage(restyled(m).replace("bench runs", "stress runs")),
        BenchError::Refused(m) => BenchError::Refused(restyled(m)),
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::safety::{PolicyOptions, SafetySettings};

    use super::*;

    #[test]
    fn wire_faults_refuse_a_proxy_outside_the_policy() {
        let dir = tempfile::tempdir().unwrap();
        let toml =
            "[workspace]\nname = \"t\"\nversion = 1\n[http]\nproxy = \"http://127.0.0.1:9\"\n";
        std::fs::write(dir.path().join("rustman.toml"), toml).unwrap();
        let ws = Workspace::load(dir.path()).unwrap();
        let policy = |target| {
            let options = PolicyOptions {
                target,
                permission: Permission::ReadOnly,
                allow_hosts: &[],
                production: None,
                environment: EnvironmentInfo::default(),
            };
            SafetyPolicy::new(&SafetySettings::default(), options).unwrap()
        };
        let err = wire_options(&ws, &policy("http://127.0.0.1:8080"), false).unwrap_err();
        assert!(err.is_refusal());
        assert_eq!(
            err.to_string(),
            "stress: wire faults: proxy not allowed (127.0.0.1)"
        );
        // The proxy is the target itself: allowed.
        let options = wire_options(&ws, &policy("http://127.0.0.1:9"), true).unwrap();
        assert!(!options.tls.verify);
    }

    #[test]
    fn bench_messages_are_restyled_for_stress() {
        let usage = BenchError::Usage(
            "bench: api/a.rman.yaml is a grpc request; bench runs http requests only".into(),
        );
        assert_eq!(
            restyle(usage).to_string(),
            "stress: api/a.rman.yaml is a grpc request; stress runs http requests only"
        );
        let refused =
            BenchError::Refused("api/a.rman.yaml: blocked: method not allowed (DELETE)".into());
        assert_eq!(
            restyle(refused).to_string(),
            "api/a.rman.yaml: blocked: method not allowed (DELETE)"
        );
    }

    fn fuzz_outcome(fuzzed_requests: u64) -> FuzzOutcome {
        let valid = |p95_ms| Valid {
            requests: 50,
            p95_ms,
            error_rate: 0.0,
        };
        let mut fuzzed = Metrics::new(LatencyKind::Corrected, Vec::new()).summary();
        fuzzed.overall.requests = fuzzed_requests;
        FuzzOutcome {
            ratio: 0.2,
            fuzzed,
            baseline_ticks: 10,
            baseline: Some(valid(100.0)),
            valid: Some(valid(400.0)),
        }
    }

    /// Valid traffic slowed down, but nothing was fuzzed (a GET-only
    /// plan): the drift is the target's own, not fuzzing's.
    #[test]
    fn valid_traffic_drift_is_not_blamed_on_fuzzing_when_nothing_was_fuzzed() {
        assert_eq!(fuzz_outcome(0).impact(), None);
        let blamed = fuzz_outcome(12).impact().expect("fuzzing degraded it");
        assert_eq!(blamed.rule, impact::RULE);
    }

    #[tokio::test]
    async fn either_token_is_cancelled_by_either_side() {
        let (a, b) = (CancellationToken::new(), CancellationToken::new());
        let first = either(&a, &b);
        a.cancel();
        assert!(first.is_cancelled());
        let (a, b) = (CancellationToken::new(), CancellationToken::new());
        let second = either(&a, &b);
        assert!(!second.is_cancelled());
        b.cancel();
        tokio::time::timeout(Duration::from_secs(5), second.cancelled())
            .await
            .expect("cancelled by the second token");
        assert!(!a.is_cancelled());
    }
}
