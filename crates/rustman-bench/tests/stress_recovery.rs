//! Kill switch, recovery and minimization against the scriptable stub
//! (add-stress-testing tasks 5.1-5.3): a target that falls over is no
//! longer loaded within 6 s, a target that heals is reported recovered and
//! one that never does gets `no-recovery`, and a crash-like fuzz finding is
//! minimized alone (or marked `load-dependent`).
//!
//! Timings are server-side or generous: the only wall-clock bound checked
//! is the spec's 6 s, with a margin for slow runners.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{fast_recovery, request, write};
use rustman_bench::stress::kill::DownReason;
use rustman_bench::stress::recovery::RecoveryStatus;
use rustman_bench::stress::run::Tuning;
use rustman_bench::stress::{
    BreakpointSpec, FailureMode, FlatSpec, ModeSpec, SoakSpec, StopReason, Stress, StressInput,
    StressOutcome, Timing,
};
use rustman_core::model::Variables;
use rustman_core::safety::Permission;
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_test_support::StressServer;
use tokio_util::sync::CancellationToken;

fn workspace(origin: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[stress]\ntargets = [\"{origin}\"]\n"),
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\n"),
    );
    dir
}

fn get(ws: &Path, file: &str, path: &str) {
    write(
        ws,
        file,
        &request("GET", &format!("{{{{base_url}}}}{path}"), ""),
    );
}

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

fn input(root: &Path, target: &str, mode: ModeSpec) -> StressInput {
    StressInput {
        workspace: Arc::new(Workspace::load(root).unwrap()),
        environment: Some("dev".into()),
        secrets: Arc::new(SecretStore::open(root, SecretBackend::File)),
        scopes: vec![PathBuf::from("api")],
        mode,
        target: target.to_owned(),
        permission: Permission::Mutations,
        production: None,
        fuzz_ratio: None,
        malformed: Vec::new(),
        wire: Vec::new(),
        allow_wire_faults: false,
        seed: Some(5),
        spec: None,
        fuzz_baseline: None,
        slos: Vec::new(),
        overrides: Variables::default(),
        timeout: Some(Duration::from_secs(5)),
        insecure: false,
        treat_throttling_as_failure: false,
        max_vus: None,
        health: None,
        recovery_timeout: None,
        interrupt: CancellationToken::new(),
        cancel: CancellationToken::new(),
        tuning: fast(),
    }
}

async fn run(input: StressInput) -> StressOutcome {
    Stress::prepare(input).unwrap().run().await.unwrap()
}

fn flat(rps: f64, secs: u64) -> ModeSpec {
    ModeSpec::Soak(SoakSpec {
        rps,
        duration: Duration::from_secs(secs),
        ..SoakSpec::default()
    })
}

/// Task 5.1: the stub falls over above 25 req/s, during the 40 req/s
/// step. Load (and every request after it: no recovery check, no
/// replays) stops within 6 s of the first refusal, as a connection break.
#[tokio::test]
async fn a_target_that_falls_over_stops_the_load_within_six_seconds() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    get(ws.path(), "api/fall.rman.yaml", "/fall?above=25");
    let mut input = input(
        ws.path(),
        &server.origin(),
        ModeSpec::Breakpoint(BreakpointSpec {
            start_rps: 10.0,
            step_factor: 2.0,
            step_duration: Duration::from_secs(3),
            refine: 0,
        }),
    );
    input.slos = vec!["p95<5s".parse().unwrap()];
    input.recovery_timeout = Some(Duration::ZERO);
    let outcome = run(input).await;
    let killed = outcome.killed.expect("the kill switch tripped");
    assert!(
        matches!(
            killed.reason,
            DownReason::Connection | DownReason::Health(FailureMode::Connection)
        ),
        "{killed}"
    );
    let bp = outcome.breakpoint.as_ref().unwrap();
    assert_eq!(bp.stop, StopReason::Killed);
    assert_eq!(bp.failure_mode, Some(FailureMode::Connection));
    assert_eq!(bp.breaking_rps, Some(killed.rps));
    let down = server.down_since().expect("the stub fell over");
    let last = server.last_arrival().unwrap();
    let loaded = last.saturating_duration_since(down);
    assert!(
        loaded <= Duration::from_secs(7),
        "load went on for {loaded:?}"
    );
    assert_eq!(outcome.recovery.status, RecoveryStatus::NotRecovered);
}

/// Task 5.2: the stub answers 503 above 8 req/s and heals 1.5 s after
/// load stops: recovered, no finding.
#[tokio::test]
async fn a_target_that_heals_is_reported_recovered() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    get(
        ws.path(),
        "api/heal.rman.yaml",
        "/heal?above=8&heal_ms=1500",
    );
    let outcome = run(input(ws.path(), &server.origin(), flat(12.0, 2))).await;
    let recovery = &outcome.recovery;
    assert_eq!(recovery.status, RecoveryStatus::Recovered, "{recovery:?}");
    assert_eq!(recovery.health.as_deref(), Some("api/heal.rman.yaml"));
    let after = recovery.after.unwrap();
    assert!(after >= Duration::from_millis(500), "{after:?}");
    assert!(after <= Duration::from_secs(10), "{after:?}");
    assert!(
        recovery
            .timeline
            .iter()
            .any(|p| p.probe.status == Some(503))
    );
    assert!(outcome.findings.is_empty(), "{:?}", outcome.findings);
}

/// Task 5.2: a stub that never heals: `no-recovery` with its last status.
#[tokio::test]
async fn a_target_that_never_heals_gets_no_recovery() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    get(
        ws.path(),
        "api/heal.rman.yaml",
        "/heal?above=8&heal_ms=600000",
    );
    let mut input = input(ws.path(), &server.origin(), flat(12.0, 2));
    input.recovery_timeout = Some(Duration::from_secs(2));
    let outcome = run(input).await;
    assert_eq!(outcome.recovery.status, RecoveryStatus::NotRecovered);
    let rules: Vec<&str> = outcome.findings.iter().map(|f| f.rule).collect();
    assert_eq!(rules, ["no-recovery"]);
    let message = &outcome.findings[0].message;
    assert!(message.contains("answered 503 in "), "{message}");
    assert!(
        message.starts_with("api/heal.rman.yaml had not recovered 2s"),
        "{message}"
    );
}

/// A fuzz run of `POST <path>` with the body `{"qty": 2, "sku": "A1"}`.
fn qty_fuzz(root: &Path, target: &str, path: &str) -> StressInput {
    write(
        root,
        "api/order.rman.yaml",
        &request(
            "POST",
            &format!("{{{{base_url}}}}{path}"),
            "body:\n  type: json\n  content: {qty: 2, sku: A1}\n",
        ),
    );
    let mut input = input(
        root,
        target,
        ModeSpec::Fuzz(FlatSpec {
            rps: 10.0,
            duration: Duration::from_secs(2),
        }),
    );
    input.fuzz_ratio = Some(1.0);
    input.fuzz_baseline = Some(Duration::from_secs(1));
    input
}

/// Task 5.3: the stub fails on `qty` being a string; the type-flipped
/// body minimizes to `{"qty":"2"}`.
#[tokio::test]
async fn a_string_quantity_is_minimized_to_that_key_alone() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    let outcome = run(qty_fuzz(ws.path(), &server.origin(), "/fuzz/qty")).await;
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule == "server-error-on-malformed-input")
        .expect("the string quantity failed");
    let min = finding.minimization.as_ref().expect("minimized");
    assert!(!min.load_dependent);
    assert_eq!(min.body.as_deref(), Some(r#"{"qty":"2"}"#));
    assert!(min.replays >= 2 && min.replays <= 50, "{}", min.replays);
}

/// Task 5.3: the stub fails on a string `qty` only above 4 req/s; alone,
/// the input gets a 400, so the finding is `load-dependent`.
#[tokio::test]
async fn a_failure_only_under_load_is_load_dependent() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    get(ws.path(), "health.rman.yaml", "/");
    let mut input = qty_fuzz(ws.path(), &server.origin(), "/fuzz/qty-load?above=4");
    input.health = Some(PathBuf::from("health.rman.yaml"));
    let outcome = run(input).await;
    assert_eq!(outcome.recovery.status, RecoveryStatus::Recovered);
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule == "server-error-on-malformed-input")
        .expect("the string quantity failed under load");
    let min = finding.minimization.as_ref().unwrap();
    assert!(min.load_dependent, "{min:?}");
    assert_eq!(min.replays, 1);
}

/// The first Ctrl+C during the recovery check: the check finishes, but
/// no minimization replay is sent afterwards.
#[tokio::test]
async fn ctrl_c_during_recovery_skips_minimization_replays() {
    let server = StressServer::start().await;
    let ws = workspace(&server.origin());
    get(ws.path(), "health.rman.yaml", "/");
    let mut input = qty_fuzz(ws.path(), &server.origin(), "/fuzz/qty");
    input.health = Some(PathBuf::from("health.rman.yaml"));
    let interrupt = input.interrupt.clone();
    let stress = Stress::prepare(input).unwrap();
    let live = stress.live();
    let path = "/fuzz/qty";
    let watcher = async {
        while live.snapshot().phase != "recovery" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        interrupt.cancel();
        // Load is over: nothing sent to the fuzzed path from now on is load.
        server.count_path(path)
    };
    let (outcome, at_interrupt) = tokio::join!(stress.run(), watcher);
    let outcome = outcome.unwrap();
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule == "server-error-on-malformed-input")
        .expect("the string quantity failed");
    assert_eq!(finding.minimization, None);
    assert_eq!(server.count_path(path), at_interrupt, "replays sent");
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w == "minimization and wire faults skipped: interrupted (Ctrl+C)"),
        "{:?}",
        outcome.warnings
    );
}
