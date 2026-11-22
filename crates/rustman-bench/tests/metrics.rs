//! Metrics, thresholds and reports against the counting server
//! (add-load-testing tasks 4.1-4.4, 5.1 and 5.3): per-second series that
//! add up to the totals, end-of-run thresholds, an abort-on-fail error
//! storm, a graceful interruption, and error samples that never show a
//! secret. Assertions are on counts, markers and generous bounds.

mod common;

use std::time::Duration;

use common::{Setup, bench, constant, request, workspace, write};
use rustman_bench::{Aborted, DataMode, Exhaust, Limit, ReportContext, Threshold, bench_report};
use rustman_report::{RenderFormat, RenderOptions, Report, render};
use rustman_test_support::BenchServer;

const SECRET: &str = "pw-canary-91fz";

fn thresholds(texts: &[&str]) -> Vec<Threshold> {
    texts.iter().map(|t| t.parse().unwrap()).collect()
}

fn seconds(n: u64) -> Limit {
    Limit::Duration(Duration::from_secs(n))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_series_adds_up_to_the_totals_with_phases_and_checks() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let checks = "assertions:\n  - status: 200\n  - body: {contains: done}\n";
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/20", checks),
    );
    let setup = Setup::new(
        &server.origin(),
        constant(2, Limit::Duration(Duration::from_millis(2500))),
    );
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let overall = &outcome.summary.overall;
    assert!(overall.requests > 10, "{overall:?}");
    let series = &outcome.series;
    assert!((2..=4).contains(&series.len()), "{series:?}");
    let counted: u64 = series.iter().map(|s| s.requests).sum();
    assert_eq!(counted, overall.requests);
    assert_eq!(series[0].second, 1);
    assert!(series[0].p95_ms >= 20.0, "{series:?}");
    assert_eq!(overall.checks.passed, overall.requests * 2);
    assert_eq!(overall.checks.failed, 0);
    assert!(overall.phases.ttfb_ms.unwrap() >= 20.0, "{overall:?}");
    assert!(overall.bytes_sent > overall.requests * 20, "{overall:?}");
    assert_eq!(overall.bytes_received, overall.requests * 4);
    assert_eq!(outcome.aborted, None);
    // About 100 req/s against the default max_rps of 200: not capped.
    assert!(
        outcome.warnings.iter().all(|w| !w.contains("max_rps")),
        "{:?}",
        outcome.warnings
    );
}

/// Task 4.2: a breached latency threshold fails the bench, a kept error
/// rate passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thresholds_are_evaluated_at_the_end() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "slow.rman.yaml",
        &request("GET", "{{base_url}}/delay/30", ""),
    );
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(10)));
    setup.thresholds = thresholds(&["p95<1ms", "error_rate<1%", "file:slow:p50>=30ms"]);
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let results = &outcome.thresholds;
    assert!(!results[0].passed, "{results:?}");
    assert!(results[0].actual.ends_with("ms"), "{results:?}");
    assert!(results[1].passed, "{results:?}");
    assert_eq!(results[1].actual, "0.00%");
    assert!(results[2].passed, "{results:?}");
    assert!(!outcome.passed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_threshold_naming_an_unknown_file_is_refused() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let mut setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(1)));
    setup.thresholds = thresholds(&["file:pay:p95<1ms"]);
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.to_string().contains("names file 'pay'"), "{err}");
    assert_eq!(server.count(), 0);
}

/// Task 4.3: an error storm breaches `error_rate<5%!`, and load stops
/// within 2 seconds of the check (the warm-up is shortened for the test).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_error_storm_aborts_the_bench() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "storm.rman.yaml",
        &request("GET", "{{base_url}}/status/500", ""),
    );
    let mut setup = Setup::new(&server.origin(), constant(2, seconds(60)));
    setup.thresholds = thresholds(&["error_rate<5%!", "p99<10s"]);
    setup.warmup = Some(Duration::ZERO);
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.aborted, Some(Aborted::Threshold));
    // First check after 1 s, then at most 1 s of drain: far below 60 s.
    assert!(
        outcome.summary.elapsed_ms < 10_000.0,
        "{}",
        outcome.summary.elapsed_ms
    );
    assert!(!outcome.thresholds[0].passed);
    assert!(outcome.thresholds[0].abort);
    assert!(outcome.thresholds[1].passed);
    assert!(!outcome.passed());
    let sent = server.count();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.count(), sent, "load continued after the bench ended");
}

/// Task 4.4 (library side): a first Ctrl+C stops new iterations and lets
/// those in flight finish; the summary covers what ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_bench_drains_and_reports() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/100", ""),
    );
    let mut setup = Setup::new(&server.origin(), constant(2, seconds(60)));
    // Long enough for two iterations even when a loaded runner makes the
    // first one take ~700 ms.
    setup.interrupt_after = Some(Duration::from_millis(2000));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.aborted, Some(Aborted::Interrupted));
    let s = &outcome.summary;
    assert!(s.elapsed_ms < 10_000.0, "{}", s.elapsed_ms);
    assert!(s.iterations >= 2, "{s:?}");
    assert_eq!(
        s.interrupted, 0,
        "in-flight iterations finish within the drain"
    );
    assert_eq!(server.count() as u64, s.overall.requests);
}

/// Task 5.3 (library side): a secret data column never reaches an error
/// sample, the report or any rendering of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn error_samples_and_reports_are_redacted() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let check = "assertions:\n  - json: {path: $.p, equals: nope}\n";
    write(
        ws.path(),
        "1-login.rman.yaml",
        &request("GET", "{{base_url}}/reflect?p={{password}}", check),
    );
    write(
        ws.path(),
        "2-fail.rman.yaml",
        &request("GET", "{{base_url}}/status/503?p={{password}}", ""),
    );
    write(ws.path(), "users.csv", &format!("password\n{SECRET}\n"));
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(30)));
    setup.data = Some(("users.csv", DataMode::Sequential, Exhaust::Recycle));
    let profile = setup.profile.clone();
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(server.hits()[0].param("p"), SECRET, "the secret is sent");
    let samples = &outcome.error_samples;
    assert_eq!(samples.len(), 40, "20 per class: {samples:?}");
    let assertion = samples.iter().find(|s| s.class == "assertion").unwrap();
    assert!(
        assertion.message.contains("«redacted:password»"),
        "{assertion:?}"
    );
    assert!(samples.iter().any(|s| s.class == "5xx"));
    let report = Report::Bench(Box::new(bench_report(
        &outcome,
        &ReportContext {
            target: &server.origin(),
            environment: Some("dev"),
            profile: &profile,
        },
    )));
    let mut outputs = vec![rustman_bench::summary::render_outcome(&outcome)];
    for format in RenderFormat::ALL {
        outputs.push(render(&report, format, &RenderOptions::default()));
    }
    for output in outputs {
        assert!(!output.contains(SECRET), "{output}");
    }
}

/// A token captured anew on every iteration (log in, then use it) is
/// redacted in every iteration's error samples, although each VU forgets
/// the previous iteration's secrets to keep memory bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_captured_every_iteration_is_redacted_in_each() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.origin()
        ),
    );
    let capture = "captures:\n  - {var: token, from: json, path: $.t}\n";
    write(
        ws.path(),
        "1-login.rman.yaml",
        &request("GET", "{{base_url}}/reflect?t=tok-{{$uuid}}", capture),
    );
    let check = "assertions:\n  - json: {path: $.t, equals: nope}\n";
    write(
        ws.path(),
        "2-me.rman.yaml",
        &request("GET", "{{base_url}}/reflect?t={{token}}", check),
    );
    let setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(30)));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let tokens: std::collections::BTreeSet<String> = server
        .hits()
        .iter()
        .filter(|h| h.param("t").starts_with("tok-"))
        .map(|h| h.param("t"))
        .collect();
    assert!(tokens.len() >= 30, "a new token per iteration: {tokens:?}");
    let samples = &outcome.error_samples;
    assert!(!samples.is_empty());
    for sample in samples {
        assert!(sample.message.contains("«redacted:token»"), "{sample:?}");
        assert!(!sample.message.contains("tok-"), "{sample:?}");
    }
}
