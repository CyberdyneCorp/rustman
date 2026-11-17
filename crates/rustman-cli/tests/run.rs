//! `rustman run` (task 6.2) and its exit codes and Ctrl+C handling (6.4):
//! collection order, `--bail`, `--filter`, `--iterations`, `--var`, the
//! folder-parallel scheduler (checked with server-side timestamps) and
//! the reporters' output.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, closed_port, request, rustman, unique, valid_report, workspace, write};
use predicates::prelude::*;
use rustman_test_support::routes::{TimedHit, timed_hits};

/// `n` requests `<folder>/<i>.rman.yaml` named `<folder>-<i>` that hit
/// `/timed/<ms>` with ids `<prefix><folder>-<i>`; `extra(i)` is appended.
fn timed_folder(
    root: &Path,
    folder: &str,
    n: usize,
    ms: u64,
    prefix: &str,
    extra: impl Fn(usize) -> String,
) {
    for i in 1..=n {
        let url = format!("{{{{base_url}}}}/timed/{ms}?id={prefix}{folder}-{i}");
        let text = request(&format!("{folder}-{i}"), &url, &extra(i));
        write(root, &format!("{folder}/{i}.rman.yaml"), &text);
    }
}

fn ids(hits: &[TimedHit], prefix: &str) -> Vec<String> {
    hits.iter()
        .map(|h| h.id.strip_prefix(prefix).unwrap().to_owned())
        .collect()
}

fn json_stdout(root: &Path, args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let output = rustman(root)
        .args(["run"])
        .args(args)
        .args(["--reporter", "json"])
        .output()
        .unwrap();
    (output.status.code(), valid_report(&output.stdout))
}

#[test]
fn a_folder_runs_in_collection_order_against_the_environment() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    timed_folder(&ws.path().join("api"), "users", 5, 10, &prefix, |_| {
        String::new()
    });
    write(
        ws.path(),
        "api/users/_collection.yaml",
        "order: ['3', '1']\n",
    );
    rustman(ws.path())
        .args(["run", "api/users", "--env", "staging"])
        .assert()
        .code(2)
        .stderr("error: environment 'staging' not found (available: dev)\n");
    rustman(ws.path())
        .args(["run", "api/users", "--env", "dev"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("PASS  api/users/3  GET 200  "))
        .stdout(predicate::str::contains(
            "5 requests: 5 passed, 0 failed, 0 errored, 0 skipped",
        ));
    let order = ids(&timed_hits(&prefix), &prefix);
    assert_eq!(
        order,
        ["users-3", "users-1", "users-2", "users-4", "users-5"]
    );
}

#[test]
fn bail_skips_the_rest_after_a_failure() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    timed_folder(ws.path(), "api", 5, 1, &prefix, |i| {
        let status = if i == 2 { 201 } else { 200 };
        format!("assertions:\n  - status: {status}\n")
    });
    let output = rustman(ws.path())
        .args(["run", "api", "--env", "dev", "--bail"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    for i in 3..=5 {
        assert!(
            stdout.contains(&format!("SKIP  api/{i}  skipped (bail)\n")),
            "{stdout}"
        );
    }
    assert!(
        stdout.contains("      status: expected 201, got 200\n"),
        "{stdout}"
    );
    assert_eq!(ids(&timed_hits(&prefix), &prefix), ["api-1", "api-2"]);
}

#[test]
fn tag_and_glob_filters_select_requests() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    timed_folder(ws.path(), "api", 4, 1, &prefix, |i| {
        let tag = if i % 2 == 1 { "smoke" } else { "slow" };
        format!("tags: [{tag}]\n")
    });
    let (code, report) = json_stdout(ws.path(), &["--env", "dev", "--filter", "tag:smoke"]);
    assert_eq!(code, Some(0));
    let ran: Vec<&str> = report["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ran, ["api/1", "api/3"]);
    assert_eq!(ids(&timed_hits(&prefix), &prefix), ["api-1", "api-3"]);
    let (_, report) = json_stdout(ws.path(), &["--env", "dev", "--filter", "**/4"]);
    assert_eq!(report["requests"][0]["id"], "api/4");
    assert_eq!(report["summary"]["total"], 1);
}

#[test]
fn a_filter_that_matches_nothing_is_a_usage_error() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/echo", ""),
    );
    rustman(ws.path())
        .args(["run", "--env", "dev", "--filter", "tag:none"])
        .assert()
        .code(2)
        .stderr("error: no requests to run (check the paths and --filter)\n");
    rustman(ws.path())
        .args(["run", "missing", "--env", "dev"])
        .assert()
        .code(2)
        .stderr("error: missing: no such file or folder\n");
}

#[test]
fn iterations_repeat_the_run_and_vars_override() {
    let server = Server::http();
    let ws = workspace(&server);
    let echo = request(
        "echo",
        "{{base_url}}/echo?who={{who}}",
        "assertions:\n  - body:\n      contains: who=flag\n",
    );
    write(ws.path(), "echo.rman.yaml", &echo);
    let (code, report) = json_stdout(
        ws.path(),
        &["--env", "dev", "--iterations", "2", "--var", "who=flag"],
    );
    assert_eq!(code, Some(0), "{report:#}");
    let requests = report["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["iteration"], 1);
    assert_eq!(requests[1]["iteration"], 2);
    assert_eq!(report["summary"]["passed"], 2);
}

#[test]
fn captures_chain_between_requests_of_a_run() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "flow/1-token.rman.yaml",
        &request(
            "token",
            "{{base_url}}/token",
            "captures:\n  - var: token\n    from: json\n    path: $.access_token\n",
        ),
    );
    write(
        ws.path(),
        "flow/2-me.rman.yaml",
        &request(
            "me",
            "{{base_url}}/me",
            "auth:\n  type: bearer\n  token: '{{token}}'\nassertions:\n  - status: 200\n",
        ),
    );
    let (code, report) = json_stdout(ws.path(), &["flow", "--env", "dev"]);
    assert_eq!(code, Some(0), "{report:#}");
    assert_eq!(report["requests"][0]["captures"][0]["outcome"], "captured");
}

/// A persisted capture is read back by a later, separate process.
#[test]
fn persisted_captures_survive_into_a_separate_run() {
    let server = Server::http();
    let ws = workspace(&server);
    let capture =
        "captures:\n  - var: token\n    from: json\n    path: $.access_token\n    persist: true\n";
    write(
        ws.path(),
        "login/token.rman.yaml",
        &request("token", "{{base_url}}/token", capture),
    );
    let me = "auth:\n  type: bearer\n  token: '{{token}}'\nassertions:\n  - status: 200\n";
    write(
        ws.path(),
        "me.rman.yaml",
        &request("me", "{{base_url}}/me", me),
    );
    rustman(ws.path())
        .args(["run", "login", "--env", "dev", "--no-history"])
        .assert()
        .success();
    let state = fs::read_to_string(ws.path().join(".rustman/state/dev.yaml")).unwrap();
    assert!(state.starts_with("token: "), "{state}");
    rustman(ws.path())
        .args(["run", "me.rman.yaml", "--env", "dev", "--no-history"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("PASS  me  GET 200"));
}

/// A request without assertions passes whatever its status, and the
/// report still shows the status.
#[test]
fn a_request_without_assertions_passes_on_a_500() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "api/1.rman.yaml",
        &request("one", "{{base_url}}/status/500", ""),
    );
    rustman(ws.path())
        .args(["run", "api", "--env", "dev", "--no-history"])
        .assert()
        .code(0)
        .stdout(predicate::str::starts_with("PASS  api/1  GET 500"));
}

/// Whether two server-side intervals overlap.
fn overlap(a: &TimedHit, b: &TimedHit) -> bool {
    let (a_end, b_end) = (a.end.unwrap(), b.end.unwrap());
    a.start < b_end && b.start < a_end
}

/// Each request of a folder starts after the previous one ended.
fn sequential(hits: &[&TimedHit]) -> bool {
    hits.windows(2).all(|w| w[1].start >= w[0].end.unwrap())
}

fn folder_hits<'a>(hits: &'a [TimedHit], prefix: &str, folder: &str) -> Vec<&'a TimedHit> {
    let start = format!("{prefix}{folder}-");
    hits.iter().filter(|h| h.id.starts_with(&start)).collect()
}

#[test]
fn parallel_folders_overlap_while_each_folder_stays_in_order() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = format!("{}-par-", unique(ws.path()));
    for folder in ["a", "b"] {
        timed_folder(ws.path(), folder, 3, 300, &prefix, |_| String::new());
    }
    let (code, report) = json_stdout(ws.path(), &["--env", "dev", "--parallel", "2"]);
    assert_eq!(code, Some(0), "{report:#}");
    let order: Vec<&str> = report["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["a/1", "a/2", "a/3", "b/1", "b/2", "b/3"]);
    let hits = timed_hits(&prefix);
    let (a, b) = (
        folder_hits(&hits, &prefix, "a"),
        folder_hits(&hits, &prefix, "b"),
    );
    assert_eq!((a.len(), b.len()), (3, 3));
    assert!(sequential(&a) && sequential(&b), "{hits:#?}");
    let concurrent = a.iter().any(|x| b.iter().any(|y| overlap(x, y)));
    assert!(concurrent, "folders a and b never overlapped: {hits:#?}");
}

#[test]
fn without_parallel_nothing_overlaps() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = format!("{}-seq-", unique(ws.path()));
    for folder in ["a", "b"] {
        timed_folder(ws.path(), folder, 2, 50, &prefix, |_| String::new());
    }
    let (code, _) = json_stdout(ws.path(), &["--env", "dev"]);
    assert_eq!(code, Some(0));
    let hits = timed_hits(&prefix);
    let all: Vec<&TimedHit> = hits.iter().collect();
    assert_eq!(all.len(), 4);
    assert!(sequential(&all), "{hits:#?}");
}

#[test]
fn a_parallel_folder_runs_its_requests_concurrently() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = format!("{}-pf-", unique(ws.path()));
    timed_folder(ws.path(), "p", 3, 300, &prefix, |_| String::new());
    write(ws.path(), "p/_collection.yaml", "parallel: true\n");
    let (code, _) = json_stdout(ws.path(), &["--env", "dev", "--parallel", "3"]);
    assert_eq!(code, Some(0));
    let hits = timed_hits(&prefix);
    let p = folder_hits(&hits, &prefix, "p");
    assert!(!sequential(&p), "{hits:#?}");
}

#[test]
fn exit_code_3_wins_over_1() {
    let server = Server::http();
    let ws = workspace(&server);
    let fails = request(
        "fails",
        "{{base_url}}/echo",
        "assertions:\n  - status: 500\n",
    );
    write(ws.path(), "1-fails.rman.yaml", &fails);
    let (code, report) = json_stdout(ws.path(), &["--env", "dev"]);
    assert_eq!(code, Some(1));
    assert_eq!(report["summary"]["failed"], 1);
    let refused = request(
        "refused",
        &format!("http://127.0.0.1:{}/", closed_port()),
        "",
    );
    write(ws.path(), "2-refused.rman.yaml", &refused);
    let (code, report) = json_stdout(ws.path(), &["--env", "dev"]);
    assert_eq!(code, Some(3));
    assert_eq!(
        (&report["summary"]["failed"], &report["summary"]["errored"]),
        (&1.into(), &1.into())
    );
    assert_eq!(report["requests"][1]["status"], "errored");
}

#[test]
fn unresolved_variables_error_without_sending() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    let url = format!("{{{{base_url}}}}/timed/1?id={prefix}{{{{missing}}}}");
    write(ws.path(), "a.rman.yaml", &request("a", &url, ""));
    let (code, report) = json_stdout(ws.path(), &["--env", "dev"]);
    assert_eq!(code, Some(3));
    assert_eq!(
        report["requests"][0]["error"],
        "unresolved variable(s): missing"
    );
    assert!(timed_hits(&prefix).is_empty());
}

#[test]
fn reports_go_to_files_and_stdout() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/echo", ""),
    );
    rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json:out/report.json"])
        .args(["--reporter", "pretty"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("PASS  a  GET 200"));
    let report = valid_report(&fs::read(ws.path().join("out/report.json")).unwrap());
    assert_eq!(report["schema"], "rustman.report/v1");
    assert_eq!(report["kind"], "run");
    assert_eq!(report["environment"], "dev");
    rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "junit-ish"])
        .assert()
        .code(2);
    rustman(ws.path())
        .args([
            "run",
            "--env",
            "dev",
            "--reporter",
            "json",
            "--reporter",
            "tap",
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            "error: only one reporter can write to stdout, got json, tap; \
             give the others a path, as in json:out/report.json\n",
        );
}

/// The CI form `--reporter junit:/dev/stdout` writes to the device.
#[cfg(unix)]
#[test]
fn a_reporter_can_write_to_dev_stdout() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/echo", ""),
    );
    let output = rustman(ws.path())
        .args([
            "run",
            "--env",
            "dev",
            "--no-history",
            "--reporter",
            "json:/dev/stdout",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report = valid_report(&output.stdout);
    assert_eq!(report["summary"]["passed"], 1);
}

#[cfg(unix)]
#[test]
fn ctrl_c_cancels_and_flushes_a_partial_report() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let server = Server::http();
    let ws = workspace(&server);
    let prefix = format!("{}-int-", unique(ws.path()));
    timed_folder(ws.path(), "api", 1, 1, &prefix, |_| String::new());
    // Long enough that only the signal can end the run.
    for i in 2..=3 {
        let url = format!("{{{{base_url}}}}/timed/60000?id={prefix}api-{i}");
        write(
            ws.path(),
            &format!("api/{i}.rman.yaml"),
            &request(&format!("api-{i}"), &url, ""),
        );
    }
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(ws.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .args(["run", "--env", "dev", "--reporter", "json:report.json"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while timed_hits(&format!("{prefix}api-2")).is_empty() {
        assert!(Instant::now() < deadline, "the run never reached api/2");
        std::thread::sleep(Duration::from_millis(20));
    }
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    let report = valid_report(&fs::read(ws.path().join("report.json")).unwrap());
    assert_eq!(report["summary"]["interrupted"], true);
    let requests = report["requests"].as_array().unwrap();
    let statuses: Vec<(&str, &serde_json::Value)> = requests
        .iter()
        .map(|r| (r["status"].as_str().unwrap(), &r["skip_reason"]))
        .collect();
    assert_eq!(
        statuses,
        [
            ("passed", &serde_json::Value::Null),
            ("skipped", &"interrupted".into()),
            ("skipped", &"interrupted".into()),
        ]
    );
    // The in-flight request was cancelled, so the server never answered it.
    let in_flight = timed_hits(&format!("{prefix}api-2"));
    assert!(in_flight[0].end.is_none());
    assert!(timed_hits(&format!("{prefix}api-3")).is_empty());
}
