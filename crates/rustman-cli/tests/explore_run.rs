//! `rustman explore run`, `status` and `export` against the `bad-api`
//! fixture (tasks 3.7, 3.9 and 3.10): the exploration directory, cleanup and
//! the left-behind list, exit codes, and regression files that fail
//! against the broken API and pass against the fixed one.

mod common;

use std::path::{Path, PathBuf};

use common::{rustman, write};
use rustman_test_support::bad_api::{BadApi, OPENAPI, TOKEN_A};
use serde_json::Value;

/// `bad-api` and the runtime it is served from.
struct Api {
    // Dropped before the runtime, which the server's task runs on.
    server: BadApi,
    _runtime: tokio::runtime::Runtime,
}

impl Api {
    fn start() -> Self {
        Self::serve(false)
    }

    fn fixed() -> Self {
        Self::serve(true)
    }

    fn serve(fixed: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(async {
            if fixed {
                BadApi::start_fixed().await
            } else {
                BadApi::start().await
            }
        });
        Self {
            server,
            _runtime: runtime,
        }
    }

    fn url(&self) -> String {
        self.server.url("")
    }
}

/// A workspace with the document at `api.yaml`, a `dev` environment whose
/// bearer token is identity A's, and a `production` one.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), "api.yaml", OPENAPI);
    write(
        dir.path(),
        "environments/dev.yaml",
        "secrets: [bearerAuth_token]\n",
    );
    write(
        dir.path(),
        "environments/production.yaml",
        "variables: {}\n",
    );
    rustman(dir.path())
        .args(["secret", "set", "bearerAuth_token", "--env", "dev"])
        .write_stdin(TOKEN_A)
        .assert()
        .success();
    dir
}

fn explore(dir: &Path, api: &Api, extra: &[&str]) -> std::process::Output {
    let target = api.url();
    let mut args = vec![
        "explore",
        "run",
        "--spec",
        "api.yaml",
        "--target",
        &target,
        "--seed",
        "7",
        "--fuzz-cases",
        "2",
        "--rate",
        "1000",
    ];
    args.extend_from_slice(extra);
    rustman(dir).args(&args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The directory of the only exploration in `dir`.
fn exploration(dir: &Path) -> PathBuf {
    let root = dir.join(".rustman").join("explorations");
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

fn report(dir: &Path) -> Value {
    let path = exploration(dir).join("report.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn id_of(dir: &Path) -> String {
    exploration(dir)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_default_run_is_read_only_and_writes_the_exploration_directory() {
    let api = Api::start();
    let ws = workspace();
    let output = explore(ws.path(), &api, &["--env", "dev"]);
    let stdout = text(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{stdout}{}",
        text(&output.stderr)
    );
    let dir = exploration(ws.path());
    let id = id_of(ws.path());
    let pattern = regex::Regex::new(r"^exp-\d{8}-[0-9a-f]{6}$").unwrap();
    assert!(pattern.is_match(&id), "{id}");
    for file in ["state.json", "journal.ndjson", "report.json", "plan.json"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    let reads = ["GET", "HEAD", "OPTIONS"];
    let hits = api.server.hits();
    let mutation = hits.iter().find(|h| !reads.contains(&h.method.as_str()));
    assert!(mutation.is_none(), "{mutation:?}");
    let report = report(ws.path());
    assert_eq!(report["schema"], "rustman.report/v1");
    assert_eq!(report["kind"], "exploration");
    assert_eq!(report["seed"], 7);
    blocked_and_skipped_probes_are_listed(&report);
    the_critical_finding_comes_first(&report);
    assert!(
        stdout.contains(&format!("exploration {id} completed")),
        "{stdout}"
    );
    assert!(stdout.contains("coverage: operations "), "{stdout}");
    let all = exploration_files(&dir) + &stdout + &text(&output.stderr);
    assert!(!all.contains(TOKEN_A), "the token leaked");
    assert!(
        all.contains("{{bearerAuth_token}}"),
        "curl keeps the reference"
    );
}

fn blocked_and_skipped_probes_are_listed(report: &Value) {
    let probes = report["probes"].as_array().unwrap();
    let blocked = report["summary"]["probes"]["blocked"].as_u64().unwrap();
    assert!(blocked > 0);
    let reasons: Vec<&str> = probes
        .iter()
        .filter(|p| p["outcome"] == "blocked")
        .map(|p| p["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons.len() as u64, blocked);
    let other = reasons.iter().find(|r| !r.ends_with("not allowed"));
    assert!(other.is_none(), "{reasons:?}");
    assert!(reasons.contains(&"mutations not allowed"));
    let unresolvable = |p: &&Value| p["operation"] == "GET /reports/{reportId}";
    let skipped = probes
        .iter()
        .filter(unresolvable)
        .any(|p| p["reason"] == "no producer");
    assert!(skipped, "the unresolvable parameter is listed");
}

fn the_critical_finding_comes_first(report: &Value) {
    let critical = &report["findings"][0];
    assert_eq!(critical["rule"], "auth-not-enforced");
    assert_eq!(critical["severity"], "critical");
    let curl = critical["reproduction"]["curl"].as_str().unwrap();
    assert!(curl.starts_with("curl "), "{critical}");
}

/// Every file of the exploration directory `dir`, concatenated.
fn exploration_files(dir: &Path) -> String {
    ["journal.ndjson", "report.json", "state.json", "plan.json"]
        .iter()
        .filter_map(|file| std::fs::read_to_string(dir.join(file)).ok())
        .collect()
}

#[test]
fn credentials_from_plain_variables_reach_no_report() {
    let api = Api::start();
    let ws = workspace();
    write(
        ws.path(),
        "environments/plain.yaml",
        &format!("variables: {{bearerAuth_token: {TOKEN_A}}}\n"),
    );
    let formats = [
        "json=out/r.json",
        "html=out/r.html",
        "markdown=out/r.md",
        "junit=out/r.xml",
        "sarif=out/r.sarif",
    ];
    let mut args = vec![
        "--env",
        "plain",
        "--probes",
        "conformance,negative-input,fuzz",
    ];
    for format in &formats {
        args.extend(["--report", format]);
    }
    let output = explore(ws.path(), &api, &args);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let mut all = exploration_files(&exploration(ws.path()));
    for name in ["r.json", "r.html", "r.md", "r.xml", "r.sarif"] {
        all += &std::fs::read_to_string(ws.path().join("out").join(name)).unwrap();
    }
    all += &text(&output.stdout);
    all += &text(&output.stderr);
    assert!(!all.contains(TOKEN_A), "the plain token leaked");
    assert!(
        all.contains("Bearer {{bearerAuth_token}}"),
        "curl keeps the reference"
    );
    assert!(
        all.contains("Bearer «redacted:"),
        "evidence shows the header"
    );
}

#[test]
fn the_exit_code_follows_fail_on() {
    let api = Api::start();
    let ws = workspace();
    let one_medium = [
        "--env",
        "dev",
        "--probes",
        "conformance",
        "--operations",
        "getUser",
    ];
    let output = explore(ws.path(), &api, &one_medium);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let report = report(ws.path());
    assert_eq!(report["summary"]["findings"]["medium"], 1, "{report}");
    assert_eq!(report["findings"][0]["rule"], "response-schema-violation");
    let mut args = one_medium.to_vec();
    args.extend(["--fail-on", "medium"]);
    let ws = workspace();
    assert_eq!(explore(ws.path(), &api, &args).status.code(), Some(1));
}

#[test]
fn invalid_input_exits_2_and_a_production_target_4() {
    let api = Api::start();
    let ws = workspace();
    let missing = explore(ws.path(), &api, &["--spec", "missing.yaml"]);
    assert_eq!(missing.status.code(), Some(2), "{}", text(&missing.stderr));
    let severity = explore(ws.path(), &api, &["--fail-on", "urgent"]);
    assert_eq!(severity.status.code(), Some(2));
    let production = explore(ws.path(), &api, &["--env", "production"]);
    assert_eq!(
        production.status.code(),
        Some(4),
        "{}",
        text(&production.stderr)
    );
    assert!(text(&production.stderr).contains("production"));
    let destructive = explore(
        ws.path(),
        &api,
        &[
            "--env",
            "production",
            "--production",
            "127.0.0.1",
            "--allow-destructive",
        ],
    );
    assert_eq!(destructive.status.code(), Some(4));
    assert!(api.server.hits().is_empty());
    assert!(!ws.path().join(".rustman/explorations").exists());
}

#[test]
fn status_shows_the_phase_progress_and_findings() {
    let api = Api::start();
    let ws = workspace();
    explore(
        ws.path(),
        &api,
        &["--env", "dev", "--probes", "conformance,auth"],
    );
    let id = id_of(ws.path());
    for args in [
        vec!["explore", "status"],
        vec!["explore", "status", id.as_str()],
    ] {
        let output = rustman(ws.path()).args(&args).output().unwrap();
        let stdout = text(&output.stdout);
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert!(
            stdout.contains(&format!("exploration {id} (completed)")),
            "{stdout}"
        );
        assert!(stdout.contains("phase: report"), "{stdout}");
        assert!(stdout.contains("1 critical, 0 high"), "{stdout}");
    }
    let json = rustman(ws.path())
        .args(["explore", "status", "--json"])
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(status["done"], status["total"]);
    assert_eq!(status["findings"]["critical"], 1);
    let unknown = rustman(ws.path())
        .args(["explore", "status", "exp-20000101-000000"])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

/// The user URLs `bad-api` created, oldest first.
fn created_users(api: &Api) -> Vec<String> {
    api.server
        .user_ids()
        .into_iter()
        .filter(|id| *id > 1)
        .map(|id| api.server.url(&format!("/users/{id}")))
        .collect()
}

#[test]
fn created_resources_are_left_behind_without_destructive() {
    let api = Api::start();
    let ws = workspace();
    let args = [
        "--env",
        "dev",
        "--allow-mutations",
        "--probes",
        "conformance,negative-input",
        "--operations",
        "createUser",
    ];
    explore(ws.path(), &api, &args);
    let created = created_users(&api);
    assert!(created.len() >= 2, "{created:?}");
    let report = report(ws.path());
    let left: Vec<&str> = report["left_behind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["url"].as_str().unwrap())
        .collect();
    assert_eq!(left, created);
    assert!(api.server.hits().iter().all(|h| h.method != "DELETE"));
}

#[test]
fn cleanup_deletes_created_resources_newest_first() {
    let api = Api::start();
    let ws = workspace();
    let args = [
        "--env",
        "dev",
        "--allow-destructive",
        "--probes",
        "conformance,negative-input",
        "--operations",
        "createUser",
    ];
    explore(ws.path(), &api, &args);
    let created = created_users(&api);
    assert!(created.len() >= 2, "{created:?}");
    let base = api.url();
    let deletes: Vec<String> = api
        .server
        .hits()
        .iter()
        .filter(|h| h.method == "DELETE")
        .map(|h| format!("{base}{}", h.target))
        .collect();
    let mut newest_first = created.clone();
    newest_first.reverse();
    assert_eq!(deletes, newest_first);
    let report = report(ws.path());
    assert_eq!(report["left_behind"], Value::Array(Vec::new()));
    assert_eq!(report["cleanup"].as_array().unwrap().len(), created.len());
}

#[test]
fn cleanup_runs_after_the_probe_budget_ran_out() {
    let api = Api::start();
    let ws = workspace();
    let args = [
        "--env",
        "dev",
        "--allow-destructive",
        "--probes",
        "conformance,negative-input",
        "--operations",
        "createUser",
        "--max-probes",
        "2",
    ];
    explore(ws.path(), &api, &args);
    let created = created_users(&api);
    assert!(!created.is_empty(), "the budgeted probes created users");
    let report = report(ws.path());
    let not_run = report["probes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["outcome"] == "not run")
        .count();
    assert!(not_run > 0, "the probe budget ran out");
    let deletes = api
        .server
        .hits()
        .iter()
        .filter(|h| h.method == "DELETE")
        .count();
    assert_eq!(deletes, created.len());
    assert_eq!(report["left_behind"], Value::Array(Vec::new()));
}

/// `rustman explore run` started in the background, killed when dropped.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_explore(dir: &Path, api: &Api, extra: &[&str]) -> Running {
    let target = api.url();
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"));
    command
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .args(["explore", "run", "--spec", "api.yaml", "--target", &target])
        .args(extra)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Running(command.spawn().unwrap())
}

/// `explore status` once it shows a running exploration with a probe done.
fn status_while_running(dir: &Path) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let output = rustman(dir).args(["explore", "status"]).output().unwrap();
        let stdout = text(&output.stdout);
        if stdout.contains("(running)") && !stdout.contains("probes: 0/") {
            return Some(stdout);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    None
}

#[test]
fn status_of_a_running_exploration() {
    let api = Api::start();
    let ws = workspace();
    // Two probes a second: the run lasts long enough to be looked at.
    let args = [
        "--env",
        "dev",
        "--probes",
        "conformance,auth",
        "--rate",
        "2",
    ];
    let running = spawn_explore(ws.path(), &api, &args);
    let stdout = status_while_running(ws.path()).expect("the exploration was seen running");
    drop(running);
    assert!(stdout.contains("phase: execute"), "{stdout}");
    let probes = regex::Regex::new(r"probes: (\d+)/(\d+) done").unwrap();
    let counts = probes.captures(&stdout).expect("progress is shown");
    let done: usize = counts[1].parse().unwrap();
    let total: usize = counts[2].parse().unwrap();
    assert!(0 < done && done < total, "{stdout}");
    assert!(stdout.contains("findings: "), "{stdout}");
}

/// The findings to export: those whose regression is one request.
const SINGLE_REQUEST_RULES: [&str; 6] = [
    "auth-not-enforced",
    "undocumented-method-allowed",
    "server-error-on-invalid-input",
    "invalid-input-accepted",
    "error-leakage",
    "permissive-cors",
];

#[test]
fn exported_regressions_fail_against_the_bad_api_and_pass_against_the_fixed_one() {
    let api = Api::start();
    let ws = workspace();
    let families = "conformance,auth,negative-input,method-not-allowed,error-leakage,security-headers,undocumented-status";
    explore(
        ws.path(),
        &api,
        &["--env", "dev", "--allow-mutations", "--probes", families],
    );
    let report = report(ws.path());
    let findings = report["findings"].as_array().unwrap();
    let mut ids: Vec<&str> = findings
        .iter()
        .filter(|f| SINGLE_REQUEST_RULES.contains(&f["rule"].as_str().unwrap()))
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    let status = findings
        .iter()
        .find(|f| f["rule"] == "undocumented-status" && f["operation"] == "GET /status")
        .unwrap();
    ids.push(status["id"].as_str().unwrap());
    let rules: std::collections::BTreeSet<&str> = findings
        .iter()
        .filter(|f| ids.contains(&f["id"].as_str().unwrap()))
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert_eq!(rules.len(), SINGLE_REQUEST_RULES.len() + 1, "{rules:?}");
    let id = id_of(ws.path());
    let exported = rustman(ws.path())
        .args([
            "explore",
            "export",
            &id,
            "--findings",
            &ids.join(","),
            "--into",
            "regressions",
        ])
        .output()
        .unwrap();
    assert!(exported.status.success(), "{}", text(&exported.stderr));
    let files = std::fs::read_dir(ws.path().join("regressions"))
        .unwrap()
        .count();
    assert_eq!(files, ids.len());
    let auth = std::fs::read_dir(ws.path().join("regressions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("auth-not-enforced"))
        .unwrap();
    let auth = std::fs::read_to_string(auth).unwrap();
    assert!(
        auth.contains("  - status:\n      - 401\n      - 403\n"),
        "{auth}"
    );
    assert!(auth.contains("url: '{{baseUrl}}/admin/users'"), "{auth}");
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/exploration.md"
    ))
    .unwrap()
    .replace("\r\n", "\n");
    assert!(
        doc.contains(&auth.replace("\r\n", "\n")),
        "docs quote the real file:\n{auth}"
    );

    let run = |api: &Api| {
        let var = format!("baseUrl={}", api.url());
        rustman(ws.path())
            .args([
                "run",
                "regressions",
                "--env",
                "dev",
                "--no-history",
                "--var",
                &var,
            ])
            .output()
            .unwrap()
    };
    let bad = run(&api);
    assert_eq!(bad.status.code(), Some(1), "{}", text(&bad.stdout));
    let fixed = Api::fixed();
    let good = run(&fixed);
    assert_eq!(good.status.code(), Some(0), "{}", text(&good.stdout));

    let all = rustman(ws.path())
        .args([
            "explore",
            "export",
            &id,
            "--findings",
            "all",
            "--into",
            "all",
        ])
        .output()
        .unwrap();
    assert!(all.status.success());
    let count = std::fs::read_dir(ws.path().join("all")).unwrap().count();
    assert_eq!(count, findings.len());
    let unknown = rustman(ws.path())
        .args([
            "explore",
            "export",
            &id,
            "--findings",
            "f999",
            "--into",
            "x",
        ])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

#[test]
fn an_exploration_exports_spans_like_a_run() {
    let api = Api::start();
    let ws = workspace();
    let port = common::closed_port();
    write(
        ws.path(),
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n[trace.export]\nenabled = true\nendpoint = \"http://127.0.0.1:{port}\"\n"
        ),
    );
    let args = [
        "--env",
        "dev",
        "--probes",
        "conformance",
        "--operations",
        "getUser",
    ];
    // A collector that is down (or a build that cannot export) only warns,
    // as it does for `rustman run`; the exploration's result is unchanged.
    let output = explore(ws.path(), &api, &args);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    let expected = if cfg!(feature = "otlp") {
        format!("warning: trace export to 127.0.0.1:{port} failed")
    } else {
        "warning: trace.export: this build cannot export spans".to_owned()
    };
    assert!(stderr.contains(&expected), "{stderr}");
    assert_eq!(stderr.matches("warning: trace").count(), 1, "{stderr}");
}
