//! Executor overhead per request (add-load-testing task 1.3, design D1):
//! one GET to a local hyper server sent by the bare `HttpEngine`, and the
//! same request sent the way a VU sends it (`Runner::send` through a
//! governed gate, then classified and recorded in the metrics).
//!
//! `cargo bench -p rustman-bench --bench executor` reports both with
//! criterion, then measures the p50 of each over interleaved samples and
//! fails when the VU path adds more than 50 µs. The result is recorded in
//! `docs/bench-internals.md`.

use std::fs;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use criterion::Criterion;
use hdrhistogram::Histogram;
use rustman_bench::metrics::{ErrorClass, LatencyKind, Metrics, Sample};
use rustman_bench::safety::{BenchGate, Safety, policy};
use rustman_core::model::Variables;
use rustman_core::run::plan;
use rustman_core::safety::{EnvironmentInfo, Governor, Limits, Permission};
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_http::{GovernedSender, HttpEngine, SendOptions};
use rustman_test_support::BenchServer;

/// The most the VU path may add to a request at p50.
const MAX_OVERHEAD: Duration = Duration::from_micros(50);
/// Interleaved samples of each path for the overhead check.
const SAMPLES: usize = 3000;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(BenchServer::start());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"b\"\nversion = 1\n",
    )
    .unwrap();
    let request = format!(
        "version: 1\nkind: http\nname: r\nurl: \"{}\"\nassertions:\n  - status: 200\n",
        server.url("/x")
    );
    fs::write(root.join("x.rman.yaml"), request).unwrap();
    let ws = Workspace::load(root).unwrap();
    let plan = plan(&ws, &[], &[]);
    let secrets = SecretStore::open(root, SecretBackend::File);
    let ctx = RunContext {
        root,
        settings: &ws.settings,
        environment: None,
        secrets: &secrets,
        in_workspace: false,
    };
    let request = &plan.requests[0];

    // The bare engine: the request resolved once, sent again and again.
    let engine = HttpEngine::new();
    let resolver = Resolver::for_request(&ws.settings, request, None, &secrets);
    let resolved = resolve_request(request, &resolver).unwrap();
    let options = SendOptions::for_request(&ws.settings, root, &resolved).unwrap();
    let engine_send = || {
        runtime.block_on(async {
            engine.send(&resolved, &options).await.unwrap();
        });
    };

    // A VU: its runner sending through a governed gate whose rate is high
    // enough never to wait, then the metrics.
    let origin = server.origin();
    let safety = Safety {
        target: &origin,
        permission: Permission::ReadOnly,
        production: None,
    };
    let policy = policy(&ws.settings.safety, safety, EnvironmentInfo::default()).unwrap();
    let governor = Governor::new(Limits {
        rate: NonZeroU32::new(10_000_000).unwrap(),
        concurrency: NonZeroU32::MIN,
        max_probes: u64::MAX,
        max_duration: Duration::from_secs(3600),
    });
    let gate = BenchGate::new(GovernedSender::new(policy, governor));
    let runner = Runner::new(
        ctx,
        RunOptions {
            overrides: Variables::default(),
            snapshots: SnapshotMode::Ci,
            gate: Some(&gate),
            ..RunOptions::default()
        },
    )
    .unwrap();
    let metrics = Metrics::new(LatencyKind::Service, vec!["x.rman.yaml".into()]);
    let vu_send = || {
        runtime.block_on(async {
            let started = Instant::now();
            let sent = runner.send(request, 1).await;
            assert!(ErrorClass::of(&sent.record).is_none(), "{:?}", sent.record);
            metrics.record(&Sample::of(0, started.elapsed(), &sent.record));
        });
    };

    let mut criterion = Criterion::default()
        .configure_from_args()
        .sample_size(50)
        .measurement_time(Duration::from_secs(3));
    criterion.bench_function("engine send", |b| b.iter(engine_send));
    criterion.bench_function("vu send", |b| b.iter(vu_send));
    criterion.final_summary();

    check_overhead(engine_send, vu_send);
}

/// Measure both paths interleaved (so drift affects them alike) and fail
/// when the VU path's p50 exceeds the engine's by more than
/// [`MAX_OVERHEAD`].
fn check_overhead(engine_send: impl Fn(), vu_send: impl Fn()) {
    let new = || Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let (mut engine, mut vu) = (new(), new());
    let time = |f: &dyn Fn()| {
        let started = Instant::now();
        f();
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
    };
    for _ in 0..SAMPLES {
        engine.record(time(&engine_send)).unwrap();
        vu.record(time(&vu_send)).unwrap();
    }
    let (engine_p50, vu_p50) = (engine.value_at_quantile(0.5), vu.value_at_quantile(0.5));
    let overhead = vu_p50.saturating_sub(engine_p50);
    println!(
        "executor overhead at p50: {overhead} µs (engine {engine_p50} µs, vu {vu_p50} µs, {SAMPLES} samples each)"
    );
    let limit = u64::try_from(MAX_OVERHEAD.as_micros()).unwrap_or(u64::MAX);
    assert!(
        overhead <= limit,
        "executor overhead at p50 is {overhead} µs (vu {vu_p50} µs - engine {engine_p50} µs, \
         {SAMPLES} samples each), above the {limit} µs limit"
    );
}
