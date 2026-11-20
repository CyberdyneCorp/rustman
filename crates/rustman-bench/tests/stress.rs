//! Stress modes against the scriptable stub (add-stress-testing tasks
//! 2.3-2.8): the breakpoint staircase and its bounds, the cap, each failure
//! mode, the first failing endpoint, spike recovery, an accelerated soak and
//! the client-saturation guard.
//!
//! The stub degrades server-side from the arrival rate it sees, with ~10%
//! margins around every threshold, and SLO windows last 1 s (instead of
//! 5 s) so the runs stay short; nothing asserts throughput or a tight
//! wall-clock bound.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{fast_recovery, request, write};
use rustman_bench::BenchError;
use rustman_bench::stress::run::Tuning;
use rustman_bench::stress::soak::SoakClock;
use rustman_bench::stress::{
    BreakpointSpec, FailureMode, ModeSpec, SoakSpec, SpikeSpec, StepVerdict, StopReason, Stress,
    StressInput, StressOutcome, Timing,
};
use rustman_core::model::Variables;
use rustman_core::safety::Permission;
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_test_support::StressServer;
use tokio_util::sync::CancellationToken;

/// A workspace whose `[stress]` lists `origin`, with `settings` appended,
/// and a `dev` environment pointing at it.
fn workspace(origin: &str, settings: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n[stress]\ntargets = [\"{origin}\"]\n{settings}"
        ),
    );
    for env in ["dev", "prod"] {
        write(
            dir.path(),
            &format!("environments/{env}.yaml"),
            &format!("variables:\n  base_url: {origin}\n"),
        );
    }
    dir
}

/// 1-second windows of two 500 ms ticks, the first window of a step a
/// warm-up.
fn fast() -> Tuning {
    Tuning {
        timing: Timing {
            tick: Duration::from_millis(500),
            window_ticks: 2,
            warmup_windows: 1,
        },
        recovery: fast_recovery(),
        ..Tuning::default()
    }
}

/// A breakpoint from `start` with `factor`, 3-second steps.
fn breakpoint(start: f64, factor: f64, refine: u32) -> ModeSpec {
    ModeSpec::Breakpoint(BreakpointSpec {
        start_rps: start,
        step_factor: factor,
        step_duration: Duration::from_secs(3),
        refine,
    })
}

struct Setup {
    mode: ModeSpec,
    slos: Vec<&'static str>,
    timeout: Duration,
    max_vus: Option<u32>,
    health: Option<std::path::PathBuf>,
    recovery_timeout: Option<Duration>,
    throttling_fails: bool,
    tuning: Tuning,
    env: &'static str,
    production: Option<String>,
}

impl Setup {
    fn new(mode: ModeSpec) -> Self {
        Self {
            mode,
            slos: Vec::new(),
            timeout: Duration::from_secs(10),
            max_vus: None,
            health: None,
            recovery_timeout: None,
            throttling_fails: false,
            tuning: fast(),
            env: "dev",
            production: None,
        }
    }

    fn slos(mut self, slos: &[&'static str]) -> Self {
        self.slos = slos.to_vec();
        self
    }
}

/// Prepare and run a stress test of every request of `root` against
/// `target`.
async fn stress(root: &Path, target: &str, setup: Setup) -> Result<StressOutcome, BenchError> {
    let input = StressInput {
        workspace: Arc::new(Workspace::load(root).unwrap()),
        environment: Some(setup.env.into()),
        secrets: Arc::new(SecretStore::open(root, SecretBackend::File)),
        scopes: Vec::new(),
        mode: setup.mode,
        target: target.to_owned(),
        permission: Permission::ReadOnly,
        production: setup.production,
        fuzz_ratio: None,
        malformed: Vec::new(),
        wire: Vec::new(),
        allow_wire_faults: false,
        seed: None,
        spec: None,
        fuzz_baseline: None,
        slos: setup.slos.iter().map(|s| s.parse().unwrap()).collect(),
        overrides: Variables::default(),
        timeout: Some(setup.timeout),
        insecure: false,
        treat_throttling_as_failure: setup.throttling_fails,
        max_vus: setup.max_vus,
        health: setup.health,
        recovery_timeout: setup.recovery_timeout,
        interrupt: CancellationToken::new(),
        cancel: CancellationToken::new(),
        tuning: setup.tuning,
    };
    Stress::prepare(input)?.run().await
}

fn get(ws: &Path, file: &str, path: &str) {
    write(
        ws,
        file,
        &request("GET", &format!("{{{{base_url}}}}{path}"), ""),
    );
}

fn rates(outcome: &StressOutcome) -> Vec<f64> {
    let bp = outcome.breakpoint.as_ref().unwrap();
    bp.steps.iter().map(|s| s.rate).collect()
}

/// Task 2.3: latency rises above 44 req/s. Steps 8, 16, 32, 64 (broken),
/// then bisection: 48 (broken), 40 (held).
#[tokio::test]
async fn the_breakpoint_climbs_then_bisects_to_the_knee() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "");
    get(ws.path(), "api/load.rman.yaml", "/load?above=44&slow=400");
    let setup = Setup::new(breakpoint(8.0, 2.0, 2)).slos(&["p95<200ms"]);
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    assert_eq!(rates(&outcome), [8.0, 16.0, 32.0, 64.0, 48.0, 40.0]);
    let bp = outcome.breakpoint.unwrap();
    let verdicts: Vec<StepVerdict> = bp.steps.iter().map(|s| s.verdict).collect();
    use StepVerdict::{Broken, Ok};
    assert_eq!(verdicts, [Ok, Ok, Ok, Broken, Broken, Ok]);
    assert_eq!(bp.stop, StopReason::Refined);
    assert_eq!(bp.max_sustainable_rps, Some(40.0));
    assert_eq!(bp.breaking_rps, Some(48.0));
    assert_eq!(bp.failure_mode, Some(FailureMode::Latency));
    assert!(!outcome.interrupted && !outcome.reached_max_duration);
}

/// A target that holds every rate: the search stops at `max_rps`.
#[tokio::test]
async fn a_breakpoint_that_never_breaks_stops_at_the_cap() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "max_rps = 20\n");
    get(ws.path(), "api/ok.rman.yaml", "/");
    let outcome = stress(
        ws.path(),
        &server.origin(),
        Setup::new(breakpoint(8.0, 2.0, 3)),
    )
    .await
    .unwrap();
    assert_eq!(rates(&outcome), [8.0, 16.0, 20.0]);
    let bp = outcome.breakpoint.unwrap();
    assert_eq!(bp.stop, StopReason::CapReached);
    assert_eq!(bp.max_sustainable_rps, Some(20.0));
    assert_eq!(bp.breaking_rps, None);
}

/// Offering exactly `max_rps` must not queue requests client-side: the
/// `[stress]` governor absorbs scheduler jitter instead of throttling the
/// offered rate below the cap, which would show up as server latency and
/// a false break at the cap. The SLO is lenient (a backlog grows by
/// seconds, a slow runner adds tens of milliseconds).
#[tokio::test]
async fn offering_the_cap_itself_is_not_throttled_into_a_false_break() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "max_rps = 30\n");
    get(ws.path(), "api/ok.rman.yaml", "/");
    let mut spec = breakpoint(30.0, 2.0, 0);
    if let ModeSpec::Breakpoint(bp) = &mut spec {
        bp.step_duration = Duration::from_secs(8);
    }
    let setup = Setup::new(spec).slos(&["p95<400ms"]);
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let bp = outcome.breakpoint.unwrap();
    assert_eq!(bp.stop, StopReason::CapReached, "{:?}", bp.steps);
    assert_eq!(bp.max_sustainable_rps, Some(30.0));
    assert_eq!(bp.breaking_rps, None);
}

/// Break the first step of a breakpoint on `path` and return its mode.
async fn first_step_mode(server: &StressServer, path: &str, setup: Setup) -> Option<FailureMode> {
    let ws = workspace(&server.origin(), "");
    get(ws.path(), "api/x.rman.yaml", path);
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let bp = outcome.breakpoint.unwrap();
    assert_eq!(bp.stop, StopReason::BrokeAtStart, "{:?}", bp.steps);
    bp.failure_mode
}

/// Task 2.4: one stub behavior per failure mode.
#[tokio::test]
async fn slow_responses_are_latency() {
    let server = StressServer::start().await;
    let setup = Setup::new(breakpoint(10.0, 2.0, 0)).slos(&["p95<100ms"]);
    let mode = first_step_mode(&server, "/load?above=0&slow=300", setup).await;
    assert_eq!(mode, Some(FailureMode::Latency));
}

#[tokio::test]
async fn server_errors_are_server_errors() {
    let server = StressServer::start().await;
    let setup = Setup::new(breakpoint(10.0, 2.0, 0));
    let mode = first_step_mode(&server, "/status/500", setup).await;
    assert_eq!(mode, Some(FailureMode::ServerErrors));
}

#[tokio::test]
async fn client_timeouts_are_timeouts() {
    let server = StressServer::start().await;
    let mut setup = Setup::new(breakpoint(10.0, 2.0, 0));
    setup.timeout = Duration::from_millis(300);
    let mode = first_step_mode(&server, "/hang", setup).await;
    assert_eq!(mode, Some(FailureMode::Timeouts));
}

#[tokio::test]
async fn closed_connections_are_connection_failures() {
    let server = StressServer::start_refusing().await;
    let setup = Setup::new(breakpoint(10.0, 2.0, 0));
    let mode = first_step_mode(&server, "/", setup).await;
    assert_eq!(mode, Some(FailureMode::Connection));
}

/// Four VUs at 800 ms per request can't keep 10 req/s: achieved falls
/// to about half of offered while latency is up.
#[tokio::test]
async fn falling_behind_the_offered_rate_is_saturation() {
    let server = StressServer::start().await;
    let mut setup = Setup::new(breakpoint(10.0, 2.0, 0)).slos(&["p95<500ms"]);
    setup.max_vus = Some(4);
    let mode = first_step_mode(&server, "/load?above=0&slow=800", setup).await;
    assert_eq!(mode, Some(FailureMode::Saturation));
}

#[tokio::test]
async fn throttling_breaks_only_when_it_counts_as_failure() {
    let server = StressServer::start().await;
    let mut setup = Setup::new(breakpoint(10.0, 2.0, 0));
    setup.throttling_fails = true;
    let mode = first_step_mode(&server, "/throttle?above=0&share=100", setup).await;
    assert_eq!(mode, Some(FailureMode::Throttled));
}

/// The spec's scenario, scaled down: 30% of requests are throttled above
/// 15 req/s. That is "rate limited at 20", not a break, and the climb goes
/// on to the cap.
#[tokio::test]
async fn a_rate_limiter_is_reported_as_rate_limited() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "max_rps = 40\n");
    get(ws.path(), "api/t.rman.yaml", "/throttle?above=15&share=30");
    let outcome = stress(
        ws.path(),
        &server.origin(),
        Setup::new(breakpoint(10.0, 2.0, 0)),
    )
    .await
    .unwrap();
    let bp = outcome.breakpoint.unwrap();
    assert_eq!(bp.rate_limited_at, Some(20.0));
    assert_eq!(bp.breaking_rps, None);
    assert_eq!(bp.max_sustainable_rps, Some(40.0));
    assert_eq!(bp.stop, StopReason::CapReached);
    assert_eq!(bp.steps[1].verdict, StepVerdict::RateLimited);
}

/// Task 2.5: `search` slows down above 10 req/s, `list` and `detail` never.
/// The steps (5, then 20 iterations per second) sit a factor of 2 on each
/// side of it, so a slow runner's bursts or stalls do not move the verdict.
#[tokio::test]
async fn the_endpoint_that_degrades_first_is_named() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "");
    get(ws.path(), "api/1-list.rman.yaml", "/load/list?above=1000");
    get(
        ws.path(),
        "api/2-search.rman.yaml",
        "/load/search?above=10&slow=400",
    );
    get(
        ws.path(),
        "api/3-detail.rman.yaml",
        "/load/detail?above=1000",
    );
    let setup = Setup::new(breakpoint(5.0, 4.0, 0)).slos(&["p95<200ms"]);
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let bp = outcome.breakpoint.unwrap();
    // Three requests per iteration: 5 then 20 iterations per second.
    assert_eq!(bp.max_sustainable_rps, Some(15.0));
    assert_eq!(bp.breaking_rps, Some(60.0));
    let broken = bp
        .steps
        .iter()
        .find(|s| s.verdict == StepVerdict::Broken)
        .unwrap();
    let first = broken.first_failing.as_ref().unwrap();
    assert_eq!(first.name(), "2-search");
    assert!(first.p95_ms >= 200.0, "{first:?}");
}

/// Task 2.6: the stub stays slow for 2 s after the spike stopped
/// overloading it; recovery is measured from the end of the spike.
///
/// Wall-clock latency is noisy on a loaded runner (a busy tick can push
/// p95 over the recovery limit for a while), so the test does not bound
/// how late recovery comes. It checks what timing noise cannot change:
/// recovery is never reported while the stub was still slow, and the
/// reported point is the first one, counted from the end of the spike, of
/// the run's own series where p95 is back within the limit.
#[tokio::test]
async fn spike_recovery_is_measured_from_the_end_of_the_spike() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "");
    let path = "/load/spike";
    get(
        ws.path(),
        "api/spike.rman.yaml",
        &format!("{path}?above=25&slow=150&base=20&hold=2000"),
    );
    let spec = SpikeSpec {
        baseline_rps: 10.0,
        baseline: Duration::from_secs(6),
        spike_factor: 5.0,
        spike_duration: Duration::from_secs(2),
    };
    let setup = Setup {
        tuning: Tuning {
            recovery: fast_recovery(),
            ..Tuning::default()
        },
        ..Setup::new(ModeSpec::Spike(spec))
    };
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let spike = outcome.spike.as_ref().unwrap();
    assert!(spike.spike_requests > 0);
    let recovered = spike.recovered_after.expect("the stub recovers");

    // Ground truth from the stub: not recovered before it answered its
    // last slow request (less one 1 s tick, as points are per tick).
    let spike_end = outcome.load_started + Duration::from_secs(8);
    let last_slow_answer = server.slow_until(path).unwrap() + Duration::from_millis(150);
    let expected = last_slow_answer.saturating_duration_since(spike_end);
    assert!(expected >= Duration::from_secs(2), "{expected:?}");
    assert!(
        recovered + Duration::from_secs(1) >= expected,
        "recovered after {recovered:?}, before the stub did ({expected:?})"
    );

    // The reported point, counted from the end of the spike (tick 8), is
    // where the series is back within the limit, and the tick before it
    // is not.
    use rustman_bench::stress::spike::{RECOVERY_FACTOR, RECOVERY_SLACK_MS};
    let base = spike.baseline_p95_ms;
    let limit = (base * RECOVERY_FACTOR).max(base + RECOVERY_SLACK_MS);
    let fine = |s: &rustman_report::bench::BenchSecond| {
        s.p95_ms <= limit && s.errors == 0 && s.requests > 0
    };
    let at = 8 + usize::try_from(recovered.as_secs()).unwrap();
    assert!(
        fine(&outcome.series[at]),
        "tick {at}: {:?} (limit {limit}ms)",
        outcome.series[at]
    );
    assert!(
        !fine(&outcome.series[at - 1]),
        "tick {}: {:?} was already within {limit}ms",
        at - 1,
        outcome.series[at - 1]
    );
}

/// Task 2.7: an accelerated soak, 600 times faster than real time: 12 s
/// of one-second buckets standing for a 2-hour soak of 10-minute buckets,
/// against a stub whose latency grows by 15 ms every second.
#[tokio::test]
async fn an_accelerated_soak_against_a_leaking_stub_reports_latency_drift() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "");
    get(ws.path(), "api/leak.rman.yaml", "/leak?base=20&per_sec=15");
    let spec = SoakSpec {
        rps: 20.0,
        duration: Duration::from_secs(12),
        drift_threshold: 0.05,
    };
    let setup = Setup {
        tuning: Tuning {
            soak: SoakClock {
                bucket: Duration::from_secs(1),
                hour: Duration::from_secs(6),
                baseline: Duration::from_secs(1),
            },
            recovery: fast_recovery(),
            ..Tuning::default()
        },
        recovery_timeout: Some(Duration::from_secs(1)),
        ..Setup::new(ModeSpec::Soak(spec))
    };
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let soak = outcome.soak.as_ref().unwrap();
    assert!(soak.p95_ms.len() >= 10, "{:?}", soak.p95_ms);
    let rules: Vec<&str> = outcome.findings.iter().map(|f| f.rule).collect();
    // A leak does not heal once load stops: the health request stays slow.
    // No other rule is asserted absent: a slow runner may add error drift.
    for rule in ["latency-drift", "no-recovery"] {
        assert!(rules.contains(&rule), "{rules:?} {:?}", soak.p95_ms);
    }
    // 15 ms per second is 90 ms per (accelerated) hour.
    assert!(soak.p95_slope > 30.0, "{}", soak.p95_slope);
}

/// Task 2.8: a scheduler pausing 100 ms after every dispatch can't offer
/// 20 req/s: the step is client-saturated, never `breaking_rps`.
#[tokio::test]
async fn a_client_that_cannot_keep_up_never_sets_the_breaking_rate() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "");
    get(ws.path(), "api/ok.rman.yaml", "/");
    let mut setup = Setup::new(breakpoint(20.0, 2.0, 0)).slos(&["p95<50ms"]);
    setup.tuning.dispatch_delay = Duration::from_millis(100);
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    let bp = outcome.breakpoint.unwrap();
    assert_eq!(bp.breaking_rps, None, "{:?}", bp.steps);
    assert_eq!(bp.stop, StopReason::ClientSaturated);
    assert_eq!(bp.steps[0].verdict, StepVerdict::ClientSaturated);
    assert!(outcome.client_saturated);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("client-saturated"))
    );
}

/// The spec's production scenario: with the host in `allow_production` and
/// `--production` repeating it, the run starts, sending valid requests.
#[tokio::test]
async fn production_with_both_opt_ins_runs() {
    let server = StressServer::start().await;
    let ws = workspace(
        &server.origin(),
        "allow_production = [\"127.0.0.1\"]\nmax_rps = 10\n",
    );
    get(ws.path(), "api/ok.rman.yaml", "/");
    let setup = Setup {
        env: "prod",
        production: Some("127.0.0.1".into()),
        ..Setup::new(breakpoint(10.0, 2.0, 0))
    };
    let outcome = stress(ws.path(), &server.origin(), setup).await.unwrap();
    assert_eq!(outcome.breakpoint.unwrap().max_sustainable_rps, Some(10.0));
    assert!(server.count() > 0);
    let refused = Setup {
        env: "prod",
        ..Setup::new(breakpoint(10.0, 2.0, 0))
    };
    let err = stress(ws.path(), &server.origin(), refused)
        .await
        .unwrap_err();
    assert!(err.is_refusal(), "{err}");
}

/// `--max-vus` above the `[stress] max_vus` cap is refused before any
/// load (a usage error, exit 2); the cap is raised only in the table.
#[tokio::test]
async fn max_vus_above_the_cap_is_refused_before_any_load() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "max_vus = 8\n");
    get(ws.path(), "api/load.rman.yaml", "/load?above=1000");
    let mut setup = Setup::new(breakpoint(10.0, 2.0, 0));
    setup.max_vus = Some(9);
    let err = stress(ws.path(), &server.origin(), setup)
        .await
        .unwrap_err();
    assert!(matches!(err, BenchError::Usage(_)), "{err}");
    assert!(
        err.to_string()
            .contains("--max-vus 9 exceeds max_vus = 8 (raise it in rustman.toml [stress])"),
        "{err}"
    );
    assert_eq!(server.count(), 0);
}
