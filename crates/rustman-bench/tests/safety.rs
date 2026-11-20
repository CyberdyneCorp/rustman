//! The bench under the safety policy and the `[bench]` caps
//! (add-load-testing tasks 2.1-2.3): refusals happen before anything is
//! sent, runtime refusals count as `blocked`, and the global governor keeps
//! the measured rate under `max_rps`.

mod common;

use std::time::Duration;

use common::{Setup, bench, constant, request, workspace, write};
use rustman_bench::{BenchError, ErrorClass, Limit};
use rustman_core::safety::Permission;
use rustman_test_support::BenchServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_host_is_refused_before_anything_is_sent() {
    let server = BenchServer::start().await;
    let ws = workspace(
        &server.origin(),
        "[safety]\nallowed_hosts = [\"https://other.example.com\"]\n",
    );
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    write(
        ws.path(),
        "b.rman.yaml",
        &request("GET", "https://other.example.com/x", ""),
    );
    let setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(4)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.is_refusal(), "{err}");
    assert!(
        err.to_string()
            .contains("blocked: host not allowed (other.example.com)"),
        "{err}"
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutation_needs_allow_mutations() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "orders.rman.yaml",
        &request("POST", "{{base_url}}/orders", ""),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(2)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.is_refusal());
    assert_eq!(
        err.to_string(),
        "orders.rman.yaml: blocked: mutations not allowed (POST)"
    );
    assert_eq!(server.count(), 0);
    let mut allowed = Setup::new(&server.origin(), constant(1, Limit::Iterations(2)));
    allowed.permission = Permission::Mutations;
    let (outcome, _) = bench(ws.path(), allowed, &[]).await.unwrap();
    assert_eq!(outcome.summary.overall.requests, 2);
    assert_eq!(server.count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_needs_allow_destructive() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "rm.rman.yaml",
        &request("DELETE", "{{base_url}}/x", ""),
    );
    let mut setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(1)));
    setup.permission = Permission::Mutations;
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.to_string().contains("destructive not allowed"), "{err}");
    assert_eq!(server.count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_needs_the_production_flag() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\n",
    );
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let mut setup = Setup::new("https://api.example.com", constant(1, Limit::Iterations(1)));
    setup.env = "prod";
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.is_refusal());
    assert!(
        err.to_string().contains("--production api.example.com"),
        "{err}"
    );
    let mut destructive = Setup::new("https://api.example.com", constant(1, Limit::Iterations(1)));
    destructive.env = "prod";
    destructive.production = Some("api.example.com");
    destructive.permission = Permission::Destructive;
    let err = bench(ws.path(), destructive, &[]).await.unwrap_err();
    assert!(err.is_refusal());
    assert!(
        err.to_string()
            .contains("destructive requests are never sent to production"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_url_known_only_at_runtime_counts_as_blocked() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let capture = "captures:\n  - var: next\n    from: json\n    path: $.next\n";
    write(
        ws.path(),
        "1-find.rman.yaml",
        &request(
            "GET",
            "{{base_url}}/reflect?next=http://other.example.com/x",
            capture,
        ),
    );
    write(
        ws.path(),
        "2-follow.rman.yaml",
        &request("GET", "{{next}}", ""),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(3)));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let follow = &outcome.summary.files[1].stats;
    assert_eq!(follow.errors_named(ErrorClass::Blocked.name()), 3);
    assert_eq!(server.count(), 3, "only the first request was sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_refuse_bigger_profiles() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let setup = Setup::new(&server.origin(), constant(100, Limit::Iterations(1)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert_eq!(
        err,
        BenchError::Usage(
            "bench: 100 VUs exceeds max_vus = 50 (raise it in rustman.toml [bench])".into()
        )
    );
    let raised = workspace(&server.origin(), "[bench]\nmax_vus = 200\n");
    write(
        raised.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let setup = Setup::new(&server.origin(), constant(100, Limit::Iterations(100)));
    let (outcome, _) = bench(raised.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.iterations, 100);
}

/// The most hits within any one second.
fn busiest_second(server: &BenchServer) -> usize {
    let times: Vec<_> = server.hits().iter().map(|h| h.at).collect();
    times
        .iter()
        .enumerate()
        .map(|(i, start)| {
            times[i..]
                .iter()
                .take_while(|t| t.duration_since(*start) < Duration::from_secs(1))
                .count()
        })
        .max()
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rate_never_exceeds_max_rps() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "[bench]\nmax_rps = 20\n");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let setup = Setup::new(
        &server.origin(),
        constant(8, Limit::Duration(Duration::from_millis(2500))),
    );
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    // A low cap, so that even a slow CI runner (about 30 requests a second
    // here with 8 VUs) presses on it: 2.5 s at 20/s is about 50 requests.
    assert!(
        outcome.summary.overall.requests > 30,
        "{:?}",
        outcome.summary
    );
    // Timestamps are taken on the server: a stall between client and
    // server can deliver at once up to one request per VU that the
    // client-side limiter had already admitted.
    let busiest = busiest_second(&server);
    assert!(busiest <= 20 + 8, "{busiest} requests in one second");
    // Every VU waited for the limit: the summary says so.
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("max_rps limit (20/s)")),
        "{:?}",
        outcome.warnings
    );
}

/// Two quick iterations: the second waits a few ms for its slot, but the
/// run is not capped by `max_rps`, and its rate is not above it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_short_run_is_not_reported_as_capped() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(2)));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let summary = &outcome.summary;
    assert_eq!(summary.overall.requests, 2);
    assert!(summary.rps <= 200.0, "{summary:?}");
    assert!(
        outcome.warnings.iter().all(|w| !w.contains("max_rps")),
        "{:?}",
        outcome.warnings
    );
}

/// A request file whose proxy is another host: the load would reach the
/// proxy, never the target, so the bench refuses it before sending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_outside_the_target_is_refused() {
    let server = BenchServer::start().await;
    let victim = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let proxy = format!("settings:\n  proxy: \"{}\"\n", victim.origin());
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", &proxy),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(5)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.is_refusal(), "{err}");
    assert!(
        err.to_string()
            .starts_with("a.rman.yaml: blocked: proxy not allowed (127.0.0.1)"),
        "{err}"
    );
    assert_eq!((server.count(), victim.count()), (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_proxy_outside_the_target_is_refused() {
    let server = BenchServer::start().await;
    let victim = BenchServer::start().await;
    let settings = format!("[http]\nproxy = \"{}\"\n", victim.origin());
    let ws = workspace(&server.origin(), &settings);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(5)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(err.to_string().contains("proxy not allowed"), "{err}");
    assert_eq!((server.count(), victim.count()), (0, 0));
    // `proxy: none` in the request bypasses it.
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", "settings:\n  proxy: none\n"),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(2)));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.overall.requests, 2);
    assert_eq!((server.count(), victim.count()), (2, 0));
}

/// A proxy known only from a capture is checked when the request is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_known_only_at_runtime_counts_as_blocked() {
    let server = BenchServer::start().await;
    let victim = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    let capture = "captures:\n  - var: next\n    from: json\n    path: $.next\n";
    write(
        ws.path(),
        "1-find.rman.yaml",
        &request(
            "GET",
            &format!("{{{{base_url}}}}/reflect?next={}", victim.origin()),
            capture,
        ),
    );
    write(
        ws.path(),
        "2-follow.rman.yaml",
        &request(
            "GET",
            "{{base_url}}/y",
            "settings:\n  proxy: \"{{next}}\"\n",
        ),
    );
    let setup = Setup::new(&server.origin(), constant(1, Limit::Iterations(3)));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    let follow = &outcome.summary.files[1].stats;
    assert_eq!(follow.errors_named(ErrorClass::Blocked.name()), 3);
    assert_eq!(server.count(), 3, "only the first request was sent");
    assert_eq!(victim.count(), 0);
    let sample = &outcome.error_samples[0];
    assert_eq!(sample.message, "blocked: proxy not allowed (127.0.0.1)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_in_the_target_are_refused() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/x", ""),
    );
    let target = server.origin().replace("http://", "http://bob:PASSCANARY@");
    let setup = Setup::new(&target, constant(1, Limit::Iterations(1)));
    let err = bench(ws.path(), setup, &[]).await.unwrap_err();
    assert!(!err.is_refusal(), "a usage error: {err}");
    assert!(!err.to_string().contains("PASSCANARY"), "{err}");
    assert!(err.to_string().contains("credentials"), "{err}");
    assert_eq!(server.count(), 0);
}
