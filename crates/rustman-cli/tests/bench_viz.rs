//! `rustman bench --viz` with a stub companion (add-load-testing tasks 6.2
//! and 7.1), on every platform.
//!
//! This test has no libtest harness (`harness = false`) so that its own
//! executable can be the stub companion: started with [`STUB`] set, it
//! answers `hello` with the views of that variable and saves every line it
//! receives to the file named by [`STUB_LOG`]. Otherwise `main` runs the
//! tests below in order and fails the process if one panics.

mod common;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{rustman, write};
use rustman_test_support::BenchServer;
use serde_json::Value;

/// Set (to a comma-separated view list, possibly empty) when this
/// executable runs as the stub companion.
const STUB: &str = "RUSTMAN_TEST_VIZ_STUB_VIEWS";
/// Where the stub saves the lines it receives.
const STUB_LOG: &str = "RUSTMAN_TEST_VIZ_STUB_LOG";

const CANARY: &str = "canary-Vz9k-bench-viz-secret";
const NO_BENCH_VIEW: &str = "viz companion has no bench view";
const NO_STRESS_VIEW: &str = "viz companion has no stress view";

type Test = (&'static str, fn());

const TESTS: &[Test] = &[
    (
        "the_bench_result_is_the_same_with_or_without_a_bench_view",
        the_bench_result_is_the_same_with_or_without_a_bench_view,
    ),
    (
        "a_missing_companion_is_a_warning",
        a_missing_companion_is_a_warning,
    ),
    (
        "a_ramping_bench_of_a_collection_end_to_end",
        a_ramping_bench_of_a_collection_end_to_end,
    ),
    (
        "a_stress_run_warns_once_without_a_stress_view",
        a_stress_run_warns_once_without_a_stress_view,
    ),
];

fn main() {
    if let Some(views) = std::env::var_os(STUB) {
        stub(&views.to_string_lossy());
        return;
    }
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let mut failed = Vec::new();
    for (name, test) in TESTS {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let ok = std::panic::catch_unwind(test).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if !failed.is_empty() {
        eprintln!("failed: {failed:?}");
        std::process::exit(101);
    }
}

/// The stub companion: `hello_ack` with `views`, then every line received
/// until stdin closes, saved at once to the log (written beside it first,
/// then renamed, so a reader never sees half of it).
fn stub(views: &str) {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let _hello = lines.next();
    let views: Vec<&str> = views.split(',').filter(|v| !v.is_empty()).collect();
    let ack = serde_json::json!({
        "type": "hello_ack",
        "protocol": "rustman-viz/1",
        "viewer": "stub",
        "views": views,
    });
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{ack}").unwrap();
    stdout.flush().unwrap();
    let received: Vec<String> = lines.map_while(Result::ok).collect();
    let log = PathBuf::from(std::env::var_os(STUB_LOG).unwrap());
    let partial = log.with_extension("partial");
    std::fs::write(&partial, received.join("\n")).unwrap();
    std::fs::rename(partial, log).unwrap();
}

/// The counting server and the runtime it is served from.
struct Counting {
    server: BenchServer,
    _runtime: tokio::runtime::Runtime,
}

impl Counting {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(BenchServer::start());
        Self {
            server,
            _runtime: runtime,
        }
    }
}

/// A workspace whose `dev` environment points at `origin` and holds the
/// secret `api_token` (the canary), with two request files that send it.
fn workspace(origin: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
    write(root, ".gitignore", ".rustman/\n");
    rustman(root)
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let request = |name: &str, path: &str| {
        format!(
            "version: 1\nkind: http\nname: {name}\nmethod: GET\n\
             url: \"{{{{base_url}}}}{path}?t={{{{api_token}}}}\"\n\
             assertions:\n  - status: 200\n"
        )
    };
    write(root, "api/1-login.rman.yaml", &request("login", "/login"));
    write(root, "api/2-list.rman.yaml", &request("list", "/items"));
    dir
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// `rustman bench` in `dir`; with `views`, `--viz` with the stub companion
/// advertising those views, logging to `log`.
fn bench(dir: &Path, args: &[&str], stub: Option<(&str, &Path)>) -> Outcome {
    let mut cmd = rustman(dir);
    cmd.arg("bench").args(args).env_remove("RUSTMAN_VIZ");
    if let Some((views, log)) = stub {
        cmd.arg("--viz")
            .env("RUSTMAN_VIZ", std::env::current_exe().unwrap())
            .env(STUB, views)
            .env(STUB_LOG, log);
    }
    let output = cmd.output().unwrap();
    let outcome = Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    for text in [&outcome.stdout, &outcome.stderr] {
        assert!(!text.contains(CANARY), "{text}");
        assert!(
            !text.contains('\x1b'),
            "no dashboard off a terminal: {text:?}"
        );
    }
    outcome
}

/// `rustman stress` in `dir`, like [`bench`].
fn stress(dir: &Path, args: &[&str], stub: Option<(&str, &Path)>) -> Outcome {
    let mut cmd = rustman(dir);
    cmd.arg("stress").args(args).env_remove("RUSTMAN_VIZ");
    if let Some((views, log)) = stub {
        cmd.arg("--viz")
            .env("RUSTMAN_VIZ", std::env::current_exe().unwrap())
            .env(STUB, views)
            .env(STUB_LOG, log);
    }
    let output = cmd.output().unwrap();
    let outcome = Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    for text in [&outcome.stdout, &outcome.stderr] {
        assert!(!text.contains(CANARY), "{text}");
        assert!(
            !text.contains('\x1b'),
            "no dashboard off a terminal: {text:?}"
        );
    }
    outcome
}

/// add-stress-testing task 6.4: a companion without the `stress` view gets
/// one warning and the run is unchanged; one with it gets the stream.
fn a_stress_run_warns_once_without_a_stress_view() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin);
    let root = ws.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[stress]\ntargets = [\"{origin}\"]\n"),
    );
    write(
        root,
        "orders/create.rman.yaml",
        "version: 1\nkind: http\nname: create\nmethod: POST\nurl: \"{{base_url}}/items?t={{api_token}}\"\n",
    );
    let args = [
        "soak",
        "orders/",
        "--env",
        "dev",
        "--target",
        &origin,
        "--rps",
        "2",
        "--duration",
        "1s",
        "--allow-mutations",
    ];
    let plain = stress(root, &args, None);
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    let without_log = root.join("stress-without.jsonl");
    let without = stress(root, &args, Some(("run_dashboard,bench", &without_log)));
    assert_eq!(without.code, plain.code, "{}", without.stderr);
    assert_eq!(
        without.stderr.matches(NO_STRESS_VIEW).count(),
        1,
        "{}",
        without.stderr
    );
    let with_log = root.join("stress-with.jsonl");
    let with = stress(root, &args, Some(("stress", &with_log)));
    assert_eq!(with.code, plain.code, "{}", with.stderr);
    assert!(!with.stderr.contains(NO_STRESS_VIEW), "{}", with.stderr);
    let messages = received(&with_log);
    assert_eq!(messages[0]["type"], "open_view");
    assert_eq!(messages[0]["view"], "stress");
    assert_eq!(messages.last().unwrap()["type"], "close");
}

/// The messages the stub received, once it has saved them.
fn received(log: &Path) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log.exists() {
        assert!(Instant::now() < deadline, "the stub never saved its log");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(log).unwrap();
    assert!(!text.contains(CANARY), "{text}");
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

fn of_type<'a>(messages: &'a [Value], kind: &str) -> Vec<&'a Value> {
    messages.iter().filter(|m| m["type"] == kind).collect()
}

/// The summary without the latency figures, which differ between runs.
fn counts(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|l| l.starts_with("iterations:") || l.starts_with("thresholds:"))
        .collect()
}

/// Task 6.2: a companion that omits the `bench` view gets a warning, once,
/// and the bench runs as without `--viz`; one that advertises it gets the
/// stream; the result and exit code are the same in all three runs.
fn the_bench_result_is_the_same_with_or_without_a_bench_view() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin);
    let args = [
        "api/",
        "--env",
        "dev",
        "--target",
        &origin,
        "--vus",
        "2",
        "--iterations",
        "10",
        "--threshold",
        "error_rate<1%",
    ];
    let plain = bench(ws.path(), &args, None);
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    assert_eq!(counting.server.count(), 20);
    without_a_bench_view(&counting, ws.path(), &args, &plain);
    with_a_bench_view(&counting, ws.path(), &args, &plain);
}

/// The second run of the test above: a companion without the view.
fn without_a_bench_view(counting: &Counting, ws: &Path, args: &[&str], plain: &Outcome) {
    let without_log = ws.join("without.jsonl");
    let without = bench(ws, args, Some(("run_dashboard", &without_log)));
    assert_eq!(without.code, plain.code, "{}", without.stderr);
    assert_eq!(counts(&without.stdout), counts(&plain.stdout));
    assert_eq!(
        without.stderr.matches(NO_BENCH_VIEW).count(),
        1,
        "{}",
        without.stderr
    );
    assert_eq!(counting.server.count(), 40);
    let messages = received(&without_log);
    assert_eq!(
        messages
            .iter()
            .map(|m| m["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["close"],
        "nothing but close without a bench view"
    );
}

/// The third run: a companion with the view gets the stream.
fn with_a_bench_view(counting: &Counting, ws: &Path, args: &[&str], plain: &Outcome) {
    let with_log = ws.join("with.jsonl");
    let with = bench(ws, args, Some(("run_dashboard,bench", &with_log)));
    assert_eq!(with.code, plain.code, "{}", with.stderr);
    assert_eq!(counts(&with.stdout), counts(&plain.stdout));
    assert!(!with.stderr.contains(NO_BENCH_VIEW), "{}", with.stderr);
    assert_eq!(counting.server.count(), 60);
    let messages = received(&with_log);
    assert_eq!(messages[0]["type"], "open_view");
    assert_eq!(messages[0]["view"], "bench");
    let finished = of_type(&messages, "bench_finished");
    assert_eq!(finished.len(), 1, "{messages:?}");
    assert_eq!(finished[0]["state"], "completed");
    assert_eq!(finished[0]["requests"], 20);
    assert_eq!(finished[0]["iterations"], 10);
    assert_eq!(finished[0]["thresholds"][0]["passed"], true);
    assert_eq!(messages.last().unwrap()["type"], "close");
}

fn a_missing_companion_is_a_warning() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin);
    let missing = ws.path().join("no-such-companion");
    let output = rustman(ws.path())
        .args(["bench", "api/", "--env", "dev", "--target", &origin])
        .args(["--iterations", "2", "--viz"])
        .env("RUSTMAN_VIZ", &missing)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr
            .matches("warning: --viz: Viz companion not found")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(counting.server.count(), 4);
}

/// Task 7.1: a ramping bench of a two-file collection against a local API,
/// with thresholds, JSON and HTML reports and `--viz`.
fn a_ramping_bench_of_a_collection_end_to_end() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin);
    let log = ws.path().join("viz.jsonl");
    let run = bench(
        ws.path(),
        &[
            "api/",
            "--env",
            "dev",
            "--target",
            &origin,
            "--executor",
            "ramping-vus",
            "--stage",
            "1s:2",
            "--stage",
            "1s:2",
            "--stage",
            "500ms:0",
            "--think-time",
            "50ms",
            "--threshold",
            "p95<5s",
            "--threshold",
            "error_rate<1%",
            "--threshold",
            "file:1-login:p99<5s",
            "--report",
            "json=out/bench.json",
            "--report",
            "html=out/bench.html",
        ],
        Some(("bench", &log)),
    );
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("api/1-login.rman.yaml"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("api/2-list.rman.yaml"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("thresholds:\n  ✓ p95<5s"),
        "{}",
        run.stdout
    );

    let requests = counting.server.hits().len();
    assert!(requests >= 4, "{requests} requests");
    every_iteration_runs_both_files(&counting);
    check_reports(ws.path(), requests);
    check_viz_stream(&log, requests);
}

fn every_iteration_runs_both_files(counting: &Counting) {
    let hits = counting.server.hits();
    let paths: Vec<&str> = hits.iter().map(|h| h.path()).collect();
    assert_eq!(
        paths.iter().filter(|p| **p == "/login").count(),
        paths.iter().filter(|p| **p == "/items").count(),
        "every iteration runs both files: {paths:?}"
    );
}

/// The JSON and HTML reports of the end-to-end bench.
fn check_reports(ws: &Path, requests: usize) {
    let text = std::fs::read_to_string(ws.join("out/bench.json")).unwrap();
    assert!(!text.contains(CANARY));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["kind"], "bench");
    assert_eq!(report["profile"]["executor"], "ramping-vus");
    assert_eq!(report["overall"]["requests"], requests);
    assert!(
        report["totals"]["iteration_duration"]["max_ms"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(report["files"].as_array().unwrap().len(), 2);
    let thresholds = report["thresholds"].as_array().unwrap();
    assert_eq!(thresholds.len(), 3);
    assert!(
        thresholds.iter().all(|t| t["passed"] == true),
        "{thresholds:?}"
    );
    let html = std::fs::read_to_string(ws.join("out/bench.html")).unwrap();
    assert!(!html.contains(CANARY));
    assert!(html.contains("<h2>Requests per second</h2>"));
    assert!(!html.contains("src=") && !html.contains("href="));
}

/// The viz stream of the end-to-end bench: a tick per second adding up to
/// the run, then `bench_finished` and `close`.
fn check_viz_stream(log: &Path, requests: usize) {
    let messages = received(log);
    assert_eq!(messages[0]["view"], "bench");
    let ticks = of_type(&messages, "bench_tick");
    assert!(!ticks.is_empty(), "{messages:?}");
    let seconds: Vec<u64> = ticks
        .iter()
        .map(|t| t["second"].as_u64().unwrap())
        .collect();
    let expected: Vec<u64> = (1..=seconds.len() as u64).collect();
    assert_eq!(seconds, expected, "one tick per second, in order");
    let tick_requests: u64 = ticks.iter().map(|t| t["requests"].as_u64().unwrap()).sum();
    assert_eq!(
        tick_requests, requests as u64,
        "the ticks add up to the run"
    );
    let finished = of_type(&messages, "bench_finished");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0]["requests"], requests);
    assert_eq!(finished[0]["thresholds"].as_array().unwrap().len(), 3);
    assert_eq!(messages.last().unwrap()["type"], "close");
}
