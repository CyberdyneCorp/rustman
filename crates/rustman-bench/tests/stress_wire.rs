//! Wire faults (add-stress-testing tasks 4.4-4.7): the raw writer against a
//! hyper server and a deliberately lax raw server, the connection bound,
//! the slow-body bounds, and the regression corpus of the old Ghostman
//! behavior (a Python fixture server).
//!
//! Every server is local. Slow-body holds its connections 3 s (not 60 s)
//! against body timeouts of 1 s or none, so the runs stay short; nothing
//! asserts a tight wall-clock bound.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::{fast_recovery, request, write};
use rustman_bench::stress::gate::{Ask, wire_consent};
use rustman_bench::stress::run::Tuning;
use rustman_bench::stress::wire::{
    self, AMBIGUOUS_FRAMING, DROPPED, NO_SLOW_CLIENT_TIMEOUT, WireOutcome, WireResult, WireTuning,
};
use rustman_bench::stress::{ModeSpec, SoakSpec, Stress, StressFinding, StressInput};
use rustman_core::model::Variables;
use rustman_core::safety::{
    EnvironmentInfo, Origin, Permission, PolicyOptions, SafetyPolicy, SafetySettings,
};
use rustman_core::stress::StressSettings;
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_http::SendOptions;
use rustman_test_support::{HyperWireServer, WireConfig, WireServer};
use tokio_util::sync::CancellationToken;

const FRAMING: [&str; 3] = ["cl-te-conflict", "invalid-chunked", "invalid-request-line"];
const HEADERS: [&str; 2] = ["oversized-headers", "many-headers"];

/// Run `families` against `origin` with at most `connections` sockets.
async fn wire_faults(
    origin: &str,
    families: &[&str],
    connections: u32,
    slow_body_for: Duration,
) -> (Vec<WireResult>, Vec<StressFinding>) {
    let settings = StressSettings {
        targets: vec![Origin::parse(origin).unwrap()],
        max_wire_connections: std::num::NonZeroU32::new(connections).unwrap(),
        ..StressSettings::default()
    };
    let policy = SafetyPolicy::new(
        &SafetySettings::default(),
        PolicyOptions {
            target: origin,
            permission: Permission::ReadOnly,
            allow_hosts: &[],
            production: None,
            environment: EnvironmentInfo::default(),
        },
    )
    .unwrap();
    let wire: Vec<String> = families.iter().map(|f| (*f).to_owned()).collect();
    let ask = Ask {
        wire: &wire,
        allow_wire_faults: true,
        ..Ask::default()
    };
    let consent = wire_consent(&settings, &policy, &ask).unwrap().unwrap();
    let tuning = WireTuning {
        read_timeout: Duration::from_secs(10),
        slow_body_for,
    };
    let stop = CancellationToken::new();
    wire::run(&consent, &SendOptions::default(), &tuning, &stop).await
}

fn rules(findings: &[StressFinding]) -> Vec<(String, &'static str)> {
    let mut rules: Vec<(String, &'static str)> = findings
        .iter()
        .map(|f| (f.family.clone().unwrap_or_default(), f.rule))
        .collect();
    rules.sort();
    rules
}

fn all(list: &[&'static str], rule: &'static str) -> Vec<(String, &'static str)> {
    list.iter().map(|f| ((*f).to_owned(), rule)).collect()
}

/// Ctrl+C ends requests already sent: a server that never answers does
/// not hold the run for the read timeout, and nothing is judged.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_ends_wire_requests_in_flight() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = accepted.clone();
    let server = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            open.push(socket);
        }
    });
    let settings = StressSettings {
        targets: vec![Origin::parse(&origin).unwrap()],
        ..StressSettings::default()
    };
    let policy = SafetyPolicy::new(
        &SafetySettings::default(),
        PolicyOptions {
            target: &origin,
            permission: Permission::ReadOnly,
            allow_hosts: &[],
            production: None,
            environment: EnvironmentInfo::default(),
        },
    )
    .unwrap();
    let wire = vec!["cl-te-conflict".to_owned(), "invalid-chunked".to_owned()];
    let ask = Ask {
        wire: &wire,
        allow_wire_faults: true,
        ..Ask::default()
    };
    let consent = wire_consent(&settings, &policy, &ask).unwrap().unwrap();
    let tuning = WireTuning {
        read_timeout: Duration::from_secs(120),
        slow_body_for: Duration::ZERO,
    };
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let probe = accepted.clone();
    tokio::spawn(async move {
        while probe.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cancel.cancel();
    });
    let options = SendOptions::default();
    let run = wire::run(&consent, &options, &tuning, &stop);
    let (results, findings) = tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("a stopped wire run ends long before its 120 s read timeout");
    assert!(findings.is_empty(), "{findings:#?}");
    assert!(results.len() <= 1, "{results:#?}");
    assert!(
        results.iter().all(|r| r.outcomes.is_empty()),
        "{results:#?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lax_server_accepts_broken_framing_and_drops_large_heads() {
    let server = WireServer::start(WireConfig {
        lax_framing: true,
        drop_heads_over: Some(64 * 1024),
        body_timeout: Some(Duration::from_secs(5)),
    })
    .await;
    let families: Vec<&str> = FRAMING.iter().chain(&HEADERS).copied().collect();
    let (results, findings) = wire_faults(&server.origin(), &families, 2, Duration::ZERO).await;
    let mut expected = all(&FRAMING, AMBIGUOUS_FRAMING);
    expected.push(("oversized-headers".into(), DROPPED));
    expected.sort();
    assert_eq!(rules(&findings), expected, "{results:#?}");
    let framing = findings
        .iter()
        .find(|f| f.rule == AMBIGUOUS_FRAMING)
        .unwrap();
    assert!(framing.message.contains("400"), "{}", framing.message);
    let dropped = findings.iter().find(|f| f.rule == DROPPED).unwrap();
    assert!(dropped.message.contains("431"), "{}", dropped.message);
    assert!(server.max_open() <= 2, "max open {}", server.max_open());
    assert_eq!(server.connections(), 15);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hyper_server_triggers_no_finding() {
    let server = HyperWireServer::start().await;
    let families: Vec<&str> = FRAMING.iter().chain(&HEADERS).copied().collect();
    let (results, findings) = wire_faults(&server.origin(), &families, 20, Duration::ZERO).await;
    assert_eq!(rules(&findings), Vec::new(), "{results:#?}");
    for result in &results {
        for outcome in &result.outcomes {
            assert!(matches!(outcome, WireOutcome::Status(_)), "{result:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_strict_raw_server_refuses_broken_framing() {
    let server = WireServer::start(WireConfig::default()).await;
    let (results, findings) = wire_faults(&server.origin(), &FRAMING, 20, Duration::ZERO).await;
    assert_eq!(rules(&findings), Vec::new());
    for result in &results {
        assert!(
            result
                .outcomes
                .iter()
                .all(|o| *o == WireOutcome::Status(400)),
            "{result:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_clients_are_timed_out_or_reported() {
    let hold = Duration::from_secs(3);
    let timing_out = WireServer::start(WireConfig {
        body_timeout: Some(Duration::from_secs(1)),
        ..WireConfig::default()
    })
    .await;
    let (results, findings) = wire_faults(&timing_out.origin(), &["slow-body"], 4, hold).await;
    assert_eq!(rules(&findings), Vec::new());
    let closed: Vec<Duration> = results[0].slow.iter().map(|c| c.unwrap()).collect();
    assert_eq!(closed.len(), 4);
    assert!(closed.iter().all(|c| *c < hold), "{closed:?}");
    assert!(timing_out.max_open() <= 4);

    let patient = WireServer::start(WireConfig::default()).await;
    let (results, findings) = wire_faults(&patient.origin(), &["slow-body"], 3, hold).await;
    assert_eq!(
        rules(&findings),
        all(&["slow-body"], NO_SLOW_CLIENT_TIMEOUT)
    );
    assert_eq!(results[0].slow, vec![None; 3]);
    assert!(
        findings[0].message.contains("all 3 slow connections"),
        "{}",
        findings[0].message
    );
    assert_eq!(patient.connections(), 3);
    assert!(patient.max_open() <= 3);
}

/// The Ghostman fixture, when a Python interpreter is found; `None`
/// skips the test unless `RUSTMAN_REQUIRE_PYTHON` is set.
fn ghostman() -> Option<(Child, String)> {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ghostman.py");
    for python in ["python3", "python"] {
        let spawned = Command::new(python)
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { continue };
        let mut line = String::new();
        let stdout = child.stdout.take().unwrap();
        if BufReader::new(stdout).read_line(&mut line).is_ok()
            && let Ok(port) = line.trim().parse::<u16>()
        {
            return Some((child, format!("http://127.0.0.1:{port}")));
        }
        let _ = child.kill();
    }
    assert!(
        std::env::var_os("RUSTMAN_REQUIRE_PYTHON").is_none(),
        "RUSTMAN_REQUIRE_PYTHON is set but no Python interpreter runs the fixture"
    );
    None
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ghostman_regression_corpus_triggers_all_three_findings() {
    let Some((child, origin)) = ghostman() else {
        eprintln!("skipped: no Python interpreter");
        return;
    };
    let _child = Killed(child);
    let families = [
        "cl-te-conflict",
        "invalid-request-line",
        "oversized-headers",
    ];
    let (results, findings) = wire_faults(&origin, &families, 5, Duration::ZERO).await;
    let mut expected = all(
        &["cl-te-conflict", "invalid-request-line"],
        AMBIGUOUS_FRAMING,
    );
    expected.push(("oversized-headers".into(), DROPPED));
    expected.sort();
    assert_eq!(rules(&findings), expected, "{results:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stress_run_sends_wire_faults_after_its_load() {
    let server = WireServer::start(WireConfig {
        lax_framing: true,
        ..WireConfig::default()
    })
    .await;
    let origin = server.origin();
    let dir = common::workspace(
        &origin,
        &format!("[stress]\ntargets = [\"{origin}\"]\nmax_wire_connections = 2\n"),
    );
    write(
        dir.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/", ""),
    );
    let input = StressInput {
        workspace: Arc::new(Workspace::load(dir.path()).unwrap()),
        environment: Some("dev".into()),
        secrets: Arc::new(SecretStore::open(dir.path(), SecretBackend::File)),
        scopes: Vec::new(),
        mode: ModeSpec::Soak(SoakSpec {
            rps: 2.0,
            duration: Duration::from_secs(1),
            ..SoakSpec::default()
        }),
        target: origin.clone(),
        permission: Permission::ReadOnly,
        production: None,
        fuzz_ratio: None,
        malformed: Vec::new(),
        wire: vec!["cl-te-conflict".into()],
        allow_wire_faults: true,
        seed: Some(1),
        spec: None,
        fuzz_baseline: None,
        slos: Vec::new(),
        overrides: Variables::default(),
        timeout: Some(Duration::from_secs(10)),
        insecure: false,
        treat_throttling_as_failure: false,
        max_vus: None,
        health: None,
        recovery_timeout: None,
        interrupt: CancellationToken::new(),
        cancel: CancellationToken::new(),
        tuning: Tuning {
            recovery: fast_recovery(),
            ..Tuning::default()
        },
    };
    let outcome = Stress::prepare(input).unwrap().run().await.unwrap();
    assert_eq!(outcome.wire.len(), 1);
    assert!(
        outcome.findings.iter().any(|f| f.rule == AMBIGUOUS_FRAMING),
        "{:#?}",
        outcome.findings
    );
}
