//! `rustman bench` (add-load-testing tasks 1.1, 2.1-2.3): exit codes and
//! messages of the refusals, which happen before anything is sent (checked
//! on a counting server), the caps, a short successful bench, and the
//! secret canary on every output.

mod common;

use common::{rustman, write};
use rustman_test_support::BenchServer;

const CANARY: &str = "canary-7Hq2-bench-secret";

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

fn request(method: &str, url: &str) -> String {
    format!("version: 1\nkind: http\nname: r\nmethod: {method}\nurl: \"{url}\"\n")
}

/// A workspace with `settings` in `rustman.toml` and a `dev` environment
/// pointing at `origin`.
fn workspace(origin: &str, settings: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n{settings}"),
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn bench(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = rustman(dir).arg("bench").args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn a_missing_target_exits_2() {
    let ws = workspace("http://127.0.0.1:9", "");
    write(
        ws.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/x"),
    );
    let (code, _, stderr) = bench(ws.path(), &["api/"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(stderr.trim_end(), "error: bench: --target is required");
}

#[test]
fn a_foreign_host_exits_4_before_any_request() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/x"),
    );
    write(
        ws.path(),
        "api/b.rman.yaml",
        &request("GET", "https://other.example.com/x"),
    );
    let (code, stdout, stderr) = bench(
        ws.path(),
        &[
            "api/",
            "--env",
            "dev",
            "--target",
            &origin,
            "--iterations",
            "5",
        ],
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("blocked: host not allowed (other.example.com)"),
        "{stderr}"
    );
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(counting.server.count(), 0);
}

#[test]
fn a_mutation_without_allow_mutations_exits_4_and_sends_nothing() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "orders.rman.yaml",
        &request("POST", "{{base_url}}/orders"),
    );
    let args = ["--env", "dev", "--target", &origin, "--iterations", "2"];
    let (code, _, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("blocked: mutations not allowed"),
        "{stderr}"
    );
    assert_eq!(counting.server.count(), 0);
    let mut allowed = args.to_vec();
    allowed.push("--allow-mutations");
    let (code, _, stderr) = bench(ws.path(), &allowed);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(counting.server.count(), 2);
}

#[test]
fn production_without_the_flag_exits_4() {
    let ws = workspace("http://127.0.0.1:9", "");
    write(
        ws.path(),
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\n",
    );
    write(
        ws.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/x"),
    );
    let (code, _, stderr) = bench(
        ws.path(),
        &[
            "api/",
            "--env",
            "prod",
            "--target",
            "https://api.example.com",
        ],
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("--production api.example.com"), "{stderr}");
}

#[test]
fn flags_cannot_exceed_the_caps() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(ws.path(), "a.rman.yaml", &request("GET", "{{base_url}}/x"));
    let args = [
        "--env",
        "dev",
        "--target",
        &origin,
        "--vus",
        "100",
        "--iterations",
        "100",
    ];
    let (code, _, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        "error: bench: 100 VUs exceeds max_vus = 50 (raise it in rustman.toml [bench])"
    );
    assert_eq!(counting.server.count(), 0);
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[bench]\nmax_vus = 200\n",
    );
    let (code, _, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains("unknown key"), "{stderr}");
    assert_eq!(counting.server.count(), 100);
}

#[test]
fn a_flag_of_another_executor_is_a_usage_error() {
    let ws = workspace("http://127.0.0.1:9", "");
    write(ws.path(), "a.rman.yaml", &request("GET", "{{base_url}}/x"));
    let (code, _, stderr) = bench(
        ws.path(),
        &[
            "--target",
            "http://127.0.0.1:9",
            "--executor",
            "ramping-vus",
            "--vus",
            "2",
        ],
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        "error: bench: --vus does not apply to the ramping-vus executor"
    );
}

#[test]
fn a_short_bench_prints_its_summary_without_secrets() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let header = "headers:\n  - name: X-Token\n    value: '{{api_token}}'\n";
    write(
        ws.path(),
        "api/1-get.rman.yaml",
        &format!(
            "{}{header}",
            request("GET", "{{base_url}}/reflect?t={{api_token}}&p={{password}}")
        ),
    );
    write(
        ws.path(),
        "api/2-fail.rman.yaml",
        &request("GET", "{{base_url}}/status/500"),
    );
    write(ws.path(), "users.csv", &format!("password\n{CANARY}-row\n"));
    let (code, stdout, stderr) = bench(
        ws.path(),
        &[
            "api/",
            "--env",
            "dev",
            "--target",
            &origin,
            "--vus",
            "2",
            "--iterations",
            "4",
            "--data",
            "users.csv",
            "-vv",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("iterations: 4 completed, 0 interrupted\n"),
        "{stdout}"
    );
    assert!(stdout.contains("requests: 8 "), "{stdout}");
    assert!(stdout.contains("api/2-fail.rman.yaml"), "{stdout}");
    assert!(stdout.contains("(5xx 4)"), "{stdout}");
    assert_eq!(counting.server.count(), 8);
    for output in [&stdout, &stderr] {
        assert!(!output.contains(CANARY), "{output}");
    }
    let hit = counting.server.hits().into_iter().next().unwrap();
    assert_eq!(
        hit.param("t"),
        CANARY,
        "the secret is sent, only never shown"
    );
}

#[test]
fn a_secret_host_is_redacted_in_the_refusal() {
    let ws = workspace("http://127.0.0.1:9", "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "https://{{token}}/x"),
    );
    write(
        ws.path(),
        "hosts.csv",
        &format!("token\n{CANARY}.example.com\n"),
    );
    let (code, stdout, stderr) = bench(
        ws.path(),
        &["--target", "http://127.0.0.1:9", "--data", "hosts.csv"],
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("blocked: host not allowed ("), "{stderr}");
    // URL parsing lowercases a host: the secret must not leak that way.
    let lower = CANARY.to_lowercase();
    for output in [&stdout, &stderr] {
        assert!(!output.to_lowercase().contains(&lower), "{output}");
    }
}

#[test]
fn a_429_from_the_target_is_a_4xx_error_and_a_warning() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/status/429"),
    );
    let (code, stdout, stderr) = bench(
        ws.path(),
        &["--env", "dev", "--target", &origin, "--iterations", "3"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("errors 100.00% (4xx 3)"), "{stdout}");
    assert!(
        stderr.contains(
            "warning: the target answered 429 Too Many Requests 3 time(s); the request rate was lowered"
        ),
        "{stderr}"
    );
}

/// Task 4.2 end to end: a breached p95 exits 1 after printing the summary,
/// a kept error rate exits 0.
#[test]
fn thresholds_decide_the_exit_code() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/20"),
    );
    let base = ["--env", "dev", "--target", &origin, "--iterations", "4"];
    let mut breached = base.to_vec();
    breached.extend(["--threshold", "p95<1ms"]);
    let (code, stdout, stderr) = bench(ws.path(), &breached);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stdout.contains("thresholds:\n  ✗ p95<1ms ("), "{stdout}");
    let mut kept = base.to_vec();
    kept.extend(["--threshold", "error_rate<1%"]);
    let (code, stdout, stderr) = bench(ws.path(), &kept);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("  ✓ error_rate<1% (0.00%)"), "{stdout}");
    let (code, _, stderr) = bench(ws.path(), &["--target", &origin, "--threshold", "p96<1ms"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("unknown metric 'p96'"), "{stderr}");
}

#[test]
fn rustman_toml_thresholds_apply_to_every_bench() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "[bench.thresholds]\np95 = \"<1ms\"\n");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/20"),
    );
    let args = ["--env", "dev", "--target", &origin, "--iterations", "2"];
    let (code, stdout, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stdout.contains("✗ p95<1ms ("), "{stdout}");
    assert!(!stderr.contains("unknown key"), "{stderr}");
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[bench.thresholds]\np95 = \"<soon\"\n",
    );
    let (code, _, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("rustman.toml [bench.thresholds]: invalid threshold 'p95<soon'"),
        "{stderr}"
    );
}

#[test]
fn rustman_toml_stages_drive_ramping_vus() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let stages = "[[bench.stages]]\nduration = \"300ms\"\ntarget = 2\n\
                  [[bench.stages]]\nduration = \"200ms\"\ntarget = 0\n";
    let ws = workspace(&origin, stages);
    write(
        ws.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/delay/20"),
    );
    let args = ["api/", "--env", "dev", "--target", &origin];
    let (code, stdout, stderr) = bench(
        ws.path(),
        &[&args[..], &["--executor", "ramping-vus"]].concat(),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains("unknown"), "{stderr}");
    assert!(stdout.contains("iterations: "), "{stdout}");
    assert!(counting.server.count() > 0);
    assert!(counting.server.max_in_flight() <= 2);
    // A --stage replaces them: its 3 VUs exceed the 2 of the file.
    let flags = ["--executor", "ramping-vus", "--stage", "300ms:3"];
    let (code, _, stderr) = bench(ws.path(), &[&args[..], &flags].concat());
    assert_eq!(code, Some(0), "{stderr}");
    assert!(counting.server.max_in_flight() <= 3);
}

#[test]
fn a_refused_connection_is_a_connect_error() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "api/a.rman.yaml",
        &request("GET", "{{base_url}}/x"),
    );
    let args = [
        "api/",
        "--env",
        "dev",
        "--target",
        &origin,
        "--iterations",
        "3",
    ];
    let (code, stdout, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("errors 100.00% (connect 3)"), "{stdout}");
}

/// The `rustman report schema` validator.
fn schema_validator(dir: &std::path::Path) -> jsonschema::Validator {
    let output = rustman(dir).args(["report", "schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// Task 5.1: the JSON report validates against `rustman report schema`,
/// `report render` reads it, and a reader without the kind refuses it.
/// Task 5.2: the HTML report is written beside it, offline.
#[test]
fn json_and_html_reports_are_written() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "api/1-ok.rman.yaml",
        &request("GET", "{{base_url}}/x"),
    );
    write(
        ws.path(),
        "api/2-fail.rman.yaml",
        &request("GET", "{{base_url}}/status/500"),
    );
    let (code, stdout, stderr) = bench(
        ws.path(),
        &[
            "api/",
            "--env",
            "dev",
            "--target",
            &origin,
            "--vus",
            "2",
            "--iterations",
            "6",
            "--threshold",
            "error_rate<60%",
            "--report",
            "json=out/bench.json",
            "--report",
            "html=out/bench.html",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.starts_with("iterations: 6 completed"), "{stdout}");
    let text = std::fs::read_to_string(ws.path().join("out/bench.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let validator = schema_validator(ws.path());
    let errors: Vec<String> = validator
        .iter_errors(&report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(report["kind"], "bench");
    assert_eq!(report["target"], origin.as_str());
    assert_eq!(report["profile"]["executor"], "constant-vus");
    assert_eq!(report["overall"]["requests"], 12);
    assert_eq!(report["files"][1]["errors"]["5xx"], 6);
    assert_eq!(report["thresholds"][0]["passed"], true);
    assert!(report.get("aborted").is_none());
    assert!(!report["series"].as_array().unwrap().is_empty());
    assert_eq!(report["error_samples"][0]["class"], "5xx");
    let html = std::fs::read_to_string(ws.path().join("out/bench.html")).unwrap();
    assert!(html.contains("<h2>Requests per second</h2>"), "{html}");
    assert!(!html.contains("src=") && !html.contains("href="));

    render_reads_it_and_refuses_an_unknown_kind(ws.path(), &text);
}

/// `report render` reads the bench report `text` (written as
/// `out/bench.json` in `dir`), and refuses it once its kind is unknown.
fn render_reads_it_and_refuses_an_unknown_kind(dir: &std::path::Path, text: &str) {
    let render = |file: &str| {
        rustman(dir)
            .args(["report", "render", file, "--format", "markdown"])
            .output()
            .unwrap()
    };
    let rendered = render("out/bench.json");
    assert_eq!(rendered.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rendered.stdout).starts_with("# Bench report\n"));
    let newer = text.replace("\"kind\": \"bench\"", "\"kind\": \"bench-next\"");
    write(dir, "out/newer.json", &newer);
    let refused = render("out/newer.json");
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("unknown report kind 'bench-next'"));
}

/// Task 5.3: a planted secret (a secret variable and a `password` data
/// column) is in no artifact: stdout, stderr, or any report format.
#[test]
fn no_artifact_shows_a_planted_secret() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let check = "assertions:\n  - json: {path: $.p, equals: nope}\n";
    write(
        ws.path(),
        "api/1-login.rman.yaml",
        &format!(
            "{}{check}",
            request("GET", "{{base_url}}/reflect?p={{password}}&t={{api_token}}")
        ),
    );
    write(
        ws.path(),
        "api/2-fail.rman.yaml",
        &request(
            "GET",
            "{{base_url}}/status/503?p={{password}}&t={{api_token}}",
        ),
    );
    write(ws.path(), "users.csv", &format!("password\n{CANARY}-row\n"));
    let formats = ["json", "html", "markdown", "junit", "sarif"];
    let mut args: Vec<String> = [
        "api/",
        "--env",
        "dev",
        "--target",
        &origin,
        "--iterations",
        "3",
        "--data",
        "users.csv",
        "--threshold",
        "checks>99%",
        "-vv",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for format in formats {
        args.extend([
            "--report".to_owned(),
            format!("{format}=out/bench.{format}"),
        ]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = bench(ws.path(), &args);
    assert_eq!(code, Some(1), "the checks threshold fails: {stderr}");
    let mut artifacts = vec![stdout, stderr];
    for format in formats {
        let path = ws.path().join(format!("out/bench.{format}"));
        artifacts.push(std::fs::read_to_string(path).unwrap());
    }
    for artifact in &artifacts {
        assert!(!artifact.contains(CANARY), "{artifact}");
    }
    assert!(
        artifacts[2].contains("«redacted:password»"),
        "{}",
        artifacts[2]
    );
    assert_eq!(
        counting.server.hits()[0].param("t"),
        CANARY,
        "the secret is sent"
    );
}

/// A `--target` with a query (`?token=`) would be copied into the report
/// and the live title: it is refused, and no report is written.
#[test]
fn a_target_with_a_query_is_refused_before_any_report() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(ws.path(), "a.rman.yaml", &request("GET", "{{base_url}}/x"));
    let target = format!("{origin}/?token={CANARY}");
    let (code, stdout, stderr) = bench(
        ws.path(),
        &[
            "--env",
            "dev",
            "--target",
            &target,
            "--iterations",
            "1",
            "--report",
            "json=out/b.json",
            "--report",
            "html=out/b.html",
        ],
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("--target must be an origin"), "{stderr}");
    assert!(
        !stdout.contains(CANARY) && !stderr.contains(CANARY),
        "{stderr}"
    );
    assert!(!ws.path().join("out/b.json").exists());
    assert!(!ws.path().join("out/b.html").exists());
    assert_eq!(counting.server.count(), 0);
}

/// A plain `--var` value in a query parameter named like a secret is not a
/// known secret, but the error samples scrub it by the parameter's name.
#[test]
fn error_samples_scrub_query_values_named_like_secrets() {
    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/status/500?password={{key}}"),
    );
    let var = format!("key={CANARY}");
    let (code, _, stderr) = bench(
        ws.path(),
        &[
            "--env",
            "dev",
            "--target",
            &origin,
            "--iterations",
            "2",
            "--var",
            &var,
            "--report",
            "json=out/b.json",
            "--report",
            "html=out/b.html",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let json = std::fs::read_to_string(ws.path().join("out/b.json")).unwrap();
    let html = std::fs::read_to_string(ws.path().join("out/b.html")).unwrap();
    for artifact in [&json, &html] {
        assert!(!artifact.contains(CANARY), "{artifact}");
    }
    assert!(json.contains("password=«redacted:password»"), "{json}");
    assert_eq!(counting.server.count(), 2, "the value is still sent");
}

/// Task 4.4: SIGINT stops new iterations, the in-flight ones drain, and a
/// partial report marked `aborted: interrupted` is written; exit 130.
#[cfg(unix)]
#[test]
fn ctrl_c_writes_a_partial_report_and_exits_130() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let counting = Counting::start();
    let origin = counting.server.origin();
    let ws = workspace(&origin, "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/delay/100"),
    );
    let child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(ws.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .args([
            "bench",
            "--env",
            "dev",
            "--target",
            &origin,
            "--vus",
            "2",
            "--duration",
            "2m",
            "--report",
            "json=out/bench.json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while counting.server.count() < 4 {
        assert!(Instant::now() < deadline, "the bench never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(
        stderr.contains("interrupted: no new iterations"),
        "{stderr}"
    );
    assert!(stdout.ends_with("aborted: interrupted\n"), "{stdout}");
    let text = std::fs::read_to_string(ws.path().join("out/bench.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["aborted"], "interrupted");
    let requests = report["overall"]["requests"].as_u64().unwrap();
    assert_eq!(requests, counting.server.count() as u64);
    assert!(report["totals"]["duration_ms"].as_f64().unwrap() < 60_000.0);
}
