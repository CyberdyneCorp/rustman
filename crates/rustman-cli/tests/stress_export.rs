//! `rustman stress` reports and `rustman stress export` (add-stress-testing
//! tasks 5.4, 6.2 and 6.6): a fuzz run against a stub that fails on a
//! string `qty` keeps its report, writes every `--report` format, and its
//! finding exports as a regression request file that fails against the
//! buggy stub and passes against the fixed one; no output, report or
//! export holds the secret the requests carried; and `report diff` exits 1
//! on a new `no-recovery`.

mod common;

use std::path::{Path, PathBuf};

use common::{rustman, write};
use rustman_test_support::StressServer;

const CANARY: &str = "canary-Qx81-stress-export-secret";

/// The stubs and the runtime they are served from.
struct Stubs {
    buggy: StressServer,
    fixed: StressServer,
    _runtime: tokio::runtime::Runtime,
}

impl Stubs {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let buggy = runtime.block_on(StressServer::start());
        let fixed = runtime.block_on(StressServer::start_fixed());
        Self {
            buggy,
            fixed,
            _runtime: runtime,
        }
    }
}

fn env_file(root: &Path, origin: &str) {
    write(
        root,
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
}

/// A workspace listing `origin` as a stress target, with the secret
/// `api_token` (the canary) sent in a header and a query parameter of
/// `POST /fuzz/qty`.
fn workspace(origin: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[stress]\ntargets = [\"{origin}\"]\n"),
    );
    env_file(root, origin);
    write(root, ".gitignore", ".rustman/\n");
    rustman(root)
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    write(
        root,
        "api/order.rman.yaml",
        "version: 1\nkind: http\nname: order\nmethod: POST\n\
         url: \"{{base_url}}/fuzz/qty?t={{api_token}}\"\n\
         headers:\n  - name: Authorization\n    value: 'Bearer {{api_token}}'\n\
         body:\n  type: json\n  content: {qty: 2, sku: A1}\n",
    );
    dir
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn rustman_in(dir: &Path, args: &[&str]) -> Output {
    let output = rustman(dir).args(args).output().unwrap();
    Output {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// The id in `stress: report saved as <id> (...)`.
fn saved_id(stderr: &str) -> String {
    let line = stderr
        .lines()
        .find_map(|l| l.strip_prefix("stress: report saved as "))
        .unwrap_or_else(|| panic!("no report id: {stderr}"));
    line.split_whitespace().next().unwrap().to_owned()
}

/// Every file under `dir` but the secret store, as text.
fn texts(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if !path.to_string_lossy().contains("secret") {
                let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
                out.push((path, text));
            }
        }
    }
    out
}

/// A fuzz run of the buggy stub with every report format; its output and
/// the id of the report it kept.
fn fuzz_with_reports(root: &Path, origin: &str) -> (Output, String) {
    let run = rustman_in(
        root,
        &[
            "stress",
            "fuzz",
            "api/",
            "--target",
            origin,
            "--env",
            "dev",
            "--rps",
            "10",
            "--duration",
            "2s",
            "--baseline",
            "1s",
            "--seed",
            "5",
            "--fuzz-ratio",
            "1",
            "--allow-mutations",
            "--report",
            "json=out/r.json",
            "--report",
            "html=out/r.html",
            "--report",
            "markdown=out/r.md",
            "--report",
            "sarif=out/r.sarif",
            "--report",
            "junit=out/r.xml",
        ],
    );
    assert_eq!(run.code, Some(1), "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("high server-error-on-malformed-input: mutation input on api/order.rman.yaml answered 500"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("    minimized ("), "{}", run.stdout);
    let id = saved_id(&run.stderr);
    let report =
        std::fs::read_to_string(root.join(format!(".rustman/stress/{id}/report.json"))).unwrap();
    assert!(report.contains("\"kind\": \"stress\""), "{report}");
    assert!(report.contains("«redacted:api_token»"), "{report}");
    (run, id)
}

/// Export every finding of run `id` into `regressions/`; the output and
/// the single regression file written.
fn export_all(root: &Path, id: &str) -> (Output, String) {
    let export = rustman_in(
        root,
        &[
            "stress",
            "export",
            id,
            "--findings",
            "all",
            "--into",
            "regressions",
        ],
    );
    assert_eq!(export.code, Some(0), "{}", export.stderr);
    let files: Vec<PathBuf> = std::fs::read_dir(root.join("regressions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let regression = std::fs::read_to_string(&files[0]).unwrap();
    (export, regression)
}

/// Neither `outputs` nor any file under `root` holds the secret.
fn assert_no_canary(root: &Path, outputs: &[&String]) {
    for output in outputs {
        assert!(!output.contains(CANARY), "{output}");
    }
    let all = texts(root);
    assert!(
        all.len() >= 8,
        "{:?}",
        all.iter().map(|t| &t.0).collect::<Vec<_>>()
    );
    for (path, text) in &all {
        assert!(!text.contains(CANARY), "{}: {text}", path.display());
    }
}

#[test]
fn a_fuzz_finding_exports_as_a_regression_and_nothing_leaks_the_secret() {
    let stubs = Stubs::start();
    let ws = workspace(&stubs.buggy.origin());
    let root = ws.path();
    let (run, id) = fuzz_with_reports(root, &stubs.buggy.origin());
    let (export, regression) = export_all(root, &id);
    assert!(regression.contains("status: '4xx'"), "{regression}");
    assert!(regression.contains(r#"{"qty":"2"}"#), "{regression}");
    assert!(regression.contains("{{api_token}}"), "{regression}");

    let buggy = rustman_in(root, &["run", "regressions/", "--env", "dev"]);
    assert_eq!(buggy.code, Some(1), "{}{}", buggy.stdout, buggy.stderr);
    env_file(root, &stubs.fixed.origin());
    let fixed = rustman_in(root, &["run", "regressions/", "--env", "dev"]);
    assert_eq!(fixed.code, Some(0), "{}{}", fixed.stdout, fixed.stderr);

    assert_no_canary(
        root,
        &[&run.stdout, &run.stderr, &export.stdout, &export.stderr],
    );
}

/// A minimal stress report with the given rules as findings.
fn report(rules: &[&str]) -> String {
    let findings: Vec<String> = rules
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            format!(
                r#"{{"id":"f{:03}","rule":"{rule}","severity":"high","operation":"api/health.rman.yaml","description":"d","message":"m","occurrences":1,"probes":[],"evidence":{{"method":"GET","url":"u","request_headers":[],"status":0,"response_headers":[],"body":""}}}}"#,
                i + 1
            )
        })
        .collect();
    format!(
        r#"{{"schema":"rustman.report/v1","kind":"stress","rustman_version":"0","id":"str-1","started_at":"","finished_at":"","target":"http://127.0.0.1","environment":null,"mode":"spike","profile":{{"settings":{{}}}},"seed":1,"slos":[],"overall":{{"requests":0,"error_rate":0.0,"errors":{{}},"latency":{{"min_ms":0.0,"avg_ms":0.0,"p50_ms":0.0,"p90_ms":0.0,"p95_ms":0.0,"p99_ms":0.0,"max_ms":0.0}},"checks":{{"passed":0,"failed":0}},"phases":{{}},"bytes_sent":0,"bytes_received":0}},"files":[],"series":[],"steps":[],"rate_limited_at":null,"killed":null,"recovery":{{"status":"recovered","after_ms":null,"baseline_ms":null,"health":null,"last_status":null,"last_latency_ms":null,"timeline":[]}},"summary":{{"findings":{{"critical":0,"high":{},"medium":0,"low":0,"info":0}},"requests":0,"fuzzed_requests":0,"duration_ms":0}},"findings":[{}]}}"#,
        rules.len(),
        findings.join(",")
    )
}

#[test]
fn report_diff_fails_on_a_new_no_recovery() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "old.json", &report(&[]));
    write(dir.path(), "new.json", &report(&["no-recovery"]));
    let same = rustman_in(dir.path(), &["report", "diff", "old.json", "old.json"]);
    assert_eq!(same.code, Some(0), "{}{}", same.stdout, same.stderr);
    let diff = rustman_in(dir.path(), &["report", "diff", "old.json", "new.json"]);
    assert_eq!(diff.code, Some(1), "{}{}", diff.stdout, diff.stderr);
    assert!(
        diff.stdout.contains("# Report diff (stress)"),
        "{}",
        diff.stdout
    );
    assert!(diff.stdout.contains("`no-recovery`"), "{}", diff.stdout);
}
