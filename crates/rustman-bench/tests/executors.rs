//! The executors against the counting server (add-load-testing tasks 1.2,
//! 3.1-3.4): VU isolation, closed-model concurrency and order, ramping
//! stages, the open model's drops and coordinated-omission correction, and
//! data feeding. Assertions are on counts and generous bounds, never on
//! tight wall-clock rates.

mod common;

use std::time::Duration;

use common::{Setup, bench, constant, request, workspace, write};
use rustman_bench::{
    ArrivalRate, ConstantVus, DataMode, ErrorClass, Exhaust, GRACEFUL_STOP, LatencyKind, Limit,
    Profile, RampingVus, ThinkTime,
};
use rustman_test_support::BenchServer;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The paths each VU requested, in order, keyed by the `vu` parameter.
fn paths_by_vu(server: &BenchServer) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut by_vu = std::collections::BTreeMap::<String, Vec<String>>::new();
    for hit in server.hits() {
        by_vu
            .entry(hit.param("vu"))
            .or_default()
            .push(hit.path().to_owned());
    }
    by_vu
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_vus_never_share_captured_variables() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let capture = "captures:\n  - var: me\n    from: json\n    path: $.user\n";
    write(
        ws.path(),
        "flow/1-login.rman.yaml",
        &request("GET", "{{base_url}}/reflect?user={{user}}", capture),
    );
    write(
        ws.path(),
        "flow/2-check.rman.yaml",
        &request("GET", "{{base_url}}/reflect?me={{me}}&user={{user}}", ""),
    );
    write(ws.path(), "users.csv", "user\nalice\nbob\n");
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(20)));
    setup.data = Some(("users.csv", DataMode::PerVu, Exhaust::Recycle));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.iterations, 20);
    assert_eq!(outcome.summary.overall.requests, 40);
    let checks: Vec<_> = server
        .hits()
        .into_iter()
        .filter(|h| h.target.contains("me="))
        .collect();
    assert_eq!(checks.len(), 20);
    for hit in &checks {
        assert_eq!(hit.param("me"), hit.param("user"), "{}", hit.target);
    }
    let users: std::collections::BTreeSet<String> =
        checks.iter().map(|h| h.param("user")).collect();
    assert_eq!(users.into_iter().collect::<Vec<_>>(), ["alice", "bob"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn constant_vus_run_concurrently_in_collection_order() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    for (file, path) in [("a", "/delay/100"), ("b", "/delay/100"), ("c", "/x")] {
        let url = format!("{{{{base_url}}}}{path}?vu={{{{vu}}}}");
        write(
            ws.path(),
            &format!("api/{file}.rman.yaml"),
            &request("GET", &url, ""),
        );
    }
    write(ws.path(), "vus.csv", "vu\n1\n2\n3\n");
    let mut setup = Setup::new(&server.origin(), constant(3, Limit::Iterations(6)));
    setup.data = Some(("vus.csv", DataMode::PerVu, Exhaust::Recycle));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.iterations, 6);
    assert_eq!(server.count(), 18);
    assert_eq!(server.max_in_flight(), 3);
    let by_vu = paths_by_vu(&server);
    assert_eq!(by_vu.len(), 3, "{by_vu:?}");
    for (vu, paths) in by_vu {
        assert_eq!(paths.len() % 3, 0, "VU {vu}: {paths:?}");
        for chunk in paths.chunks(3) {
            assert_eq!(chunk, ["/delay/100", "/delay/100", "/x"], "VU {vu}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_duration_limit_and_think_time_bound_the_iterations() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let profile = Profile::ConstantVus(ConstantVus {
        vus: 1,
        limit: Limit::Duration(ms(1000)),
        think: ThinkTime::Fixed(ms(300)),
    });
    let started = std::time::Instant::now();
    let (outcome, _) = bench(ws.path(), Setup::new(&server.origin(), profile), &[])
        .await
        .unwrap();
    let iterations = outcome.summary.iterations;
    assert!((2..=5).contains(&iterations), "{iterations}");
    assert!(started.elapsed() < ms(3000), "{:?}", started.elapsed());
    assert_eq!(outcome.summary.latency_kind, LatencyKind::Service);
}

/// The last iteration is claimed before thinking: an iterations limit
/// with a long think time ends right after its last request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_think_time_after_the_last_iteration() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let profile = Profile::ConstantVus(ConstantVus {
        vus: 1,
        limit: Limit::Iterations(1),
        think: ThinkTime::Fixed(ms(4000)),
    });
    let started = std::time::Instant::now();
    let (outcome, _) = bench(ws.path(), Setup::new(&server.origin(), profile), &[])
        .await
        .unwrap();
    assert_eq!(outcome.summary.iterations, 1);
    assert!(started.elapsed() < ms(2000), "{:?}", started.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ramping_vus_follow_the_stages() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/20", ""),
    );
    let profile = Profile::RampingVus(RampingVus {
        // The sampler's clock starts before the ramp's (the runners are
        // built in between). A 2-second ramp moves one VU per 500 ms, so
        // its mid-ramp sample tolerates 500 ms of offset within the ±1
        // margin; a 2-second plateau keeps its samples away from either
        // ramp, so a late sampler on a slow runner still sees 4 VUs.
        stages: ["2s:4", "2s:4", "1s:0"]
            .map(|s| s.parse().unwrap())
            .to_vec(),
        think: ThinkTime::None,
        graceful_stop: GRACEFUL_STOP,
    });
    let at = [ms(1000), ms(2500), ms(3000), ms(3500), ms(5600)];
    let (outcome, samples) = bench(ws.path(), Setup::new(&server.origin(), profile), &at)
        .await
        .unwrap();
    let expected = [2, 4, 4, 4, 0];
    for ((sample, want), at) in samples.iter().zip(expected).zip(at) {
        assert!(
            sample.abs_diff(want) <= 1,
            "at {at:?}: {sample} VUs, expected {want}"
        );
    }
    assert!(server.max_in_flight() <= 4, "{}", server.max_in_flight());
    assert!(outcome.summary.iterations > 0);
    assert_eq!(outcome.summary.interrupted, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exhausted_pool_drops_iterations() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/500", ""),
    );
    let profile = Profile::ConstantArrivalRate(ArrivalRate {
        rate: 20,
        duration: ms(2000),
        max_vus: 2,
    });
    let (outcome, samples) = bench(
        ws.path(),
        Setup::new(&server.origin(), profile),
        &[ms(800), ms(1300)],
    )
    .await
    .unwrap();
    // Both VUs of the saturated pool are busy: they count as active, live
    // and in the per-second series.
    assert!(samples.iter().all(|&n| n > 0), "{samples:?}");
    assert!(
        outcome.series.iter().any(|second| second.active_vus > 0),
        "{:?}",
        outcome.series
    );
    let s = &outcome.summary;
    assert_eq!(s.latency_kind, LatencyKind::Corrected);
    assert_eq!(
        s.scheduled,
        s.iterations + s.interrupted + s.dropped,
        "{s:?}"
    );
    assert!((35..=45).contains(&s.scheduled), "{s:?}");
    // Each of the 2 VUs runs at most one 500 ms iteration at a time.
    assert!(s.iterations <= 10, "{s:?}");
    assert!(s.dropped >= s.scheduled - 10, "{s:?}");
    assert_eq!(server.count() as u64, s.iterations + s.interrupted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrected_latency_includes_a_server_stall() {
    let server = BenchServer::start().await;
    server.stall_once(5, ms(2000));
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let profile = Profile::ConstantArrivalRate(ArrivalRate {
        rate: 10,
        duration: ms(3000),
        max_vus: 40,
    });
    let (outcome, _) = bench(ws.path(), Setup::new(&server.origin(), profile), &[])
        .await
        .unwrap();
    let s = &outcome.summary;
    assert_eq!(s.dropped, 0, "{s:?}");
    assert!(s.iterations >= 25, "{s:?}");
    let latency = s.overall.latency;
    assert!(latency.p99_ms >= 1500.0, "{latency:?}");
    // The stall plus the queueing behind it, with room for a slow runner;
    // an uncorrected or double-counted latency would be far off either way.
    assert!(latency.max_ms < 4000.0, "{latency:?}");
    // Most iterations scheduled during the stall waited for it.
    assert!(latency.p50_ms >= 200.0, "{latency:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_data_stops_when_the_rows_run_out() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x?row={{n}}", ""),
    );
    write(ws.path(), "rows.json", r#"[{"n": 1}, {"n": 2}, {"n": 3}]"#);
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(50)));
    setup.data = Some(("rows.json", DataMode::Sequential, Exhaust::Stop));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.iterations, 3);
    let mut rows: Vec<String> = server.hits().iter().map(|h| h.param("row")).collect();
    rows.sort();
    assert_eq!(rows, ["1", "2", "3"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_data_uses_rows_of_the_file() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x?row={{n}}", ""),
    );
    write(ws.path(), "rows.csv", "n\na\nb\n");
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(40)));
    setup.data = Some(("rows.csv", DataMode::Random, Exhaust::Stop));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.iterations, 40);
    let rows: std::collections::BTreeSet<String> =
        server.hits().iter().map(|h| h.param("row")).collect();
    assert_eq!(rows.into_iter().collect::<Vec<_>>(), ["a", "b"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_file_errors_are_counted_by_class() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    write(
        ws.path(),
        "b.rman.yaml",
        &request("GET", "{{base_url}}/status/503", ""),
    );
    let (outcome, _) = bench(
        ws.path(),
        Setup::new(&server.origin(), constant(2, Limit::Iterations(4))),
        &[],
    )
    .await
    .unwrap();
    let files = &outcome.summary.files;
    assert_eq!(files[0].file, "a.rman.yaml");
    assert_eq!(files[0].stats.requests, 4);
    assert_eq!(files[0].stats.error_rate, 0.0);
    assert_eq!(files[1].stats.errors_named(ErrorClass::Status5xx.name()), 4);
    assert_eq!(outcome.summary.overall.error_rate, 0.5);
}
