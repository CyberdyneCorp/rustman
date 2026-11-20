//! `rustman monitor list` and `rustman monitor run` (add-scheduled-monitors
//! sections 1-3) against the in-process test server: loading errors, the
//! one-shot exit codes, the read-only policy, the whole-run deadline, the
//! stored results, their redaction and the git-ignore warning.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, closed_port, request, rustman, write};

const CANARY: &str = "canary-Mon1tor-s3cret";

/// A workspace with a `prod` environment pointing at `base_url`, and the
/// store git-ignored.
fn workspace(base_url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/prod.yaml",
        &format!("variables:\n  base_url: {base_url}\nsecrets: [token]\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn monitor(ws: &Path, name: &str, extra: &str) {
    write(
        ws,
        &format!("monitors/{name}.rman-monitor.yaml"),
        &format!(
            "version: 1\nname: {name}\ntargets: [health/]\nenvironment: prod\nschedule: {{every: 5m}}\n{extra}"
        ),
    );
}

fn health(ws: &Path, file: &str, url: &str, extra: &str) {
    write(
        ws,
        &format!("health/{file}.rman.yaml"),
        &request(file, url, extra),
    );
}

/// Every stored raw result of `name`, as JSON values.
fn stored(ws: &Path, name: &str) -> Vec<serde_json::Value> {
    let dir = ws.join(".rustman/monitors").join(name).join("raw");
    let mut lines = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        lines.extend(text.lines().map(|l| serde_json::from_str(l).unwrap()));
    }
    lines
}

fn run(ws: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = rustman(ws)
        .args(["monitor", "run"])
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn list_shows_the_schedule_and_environment() {
    let ws = workspace("http://127.0.0.1:1");
    monitor(ws.path(), "api-health", "");
    let output = rustman(ws.path())
        .args(["monitor", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let row = stdout
        .lines()
        .find(|l| l.starts_with("api-health"))
        .unwrap_or_else(|| panic!("{stdout}"));
    let columns: Vec<&str> = row
        .split("  ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(columns, ["api-health", "every 5m", "prod", "health/"]);
    let output = rustman(ws.path())
        .args(["monitor", "list", "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json[0]["schedule"], "every 5m");
    assert_eq!(json[0]["environment"], "prod");
    assert_eq!(json[0]["timeout_ms"], 60_000);
}

#[test]
fn invalid_monitor_files_exit_2_with_the_line() {
    let ws = workspace("http://127.0.0.1:1");
    write(
        ws.path(),
        "monitors/api-health.rman-monitor.yaml",
        "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {every: 5m, cron: \"* * * * *\"}\n",
    );
    let output = rustman(ws.path())
        .args(["monitor", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap().replace('\\', "/");
    assert_eq!(
        stderr.trim_end(),
        "error: monitors/api-health.rman-monitor.yaml:5: schedule must set exactly one of cron or every"
    );
    write(
        ws.path(),
        "monitors/api-health.rman-monitor.yaml",
        "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {every: 5s}\n",
    );
    let (code, _, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("every must be at least 30s"), "{stderr}");
}

#[test]
fn a_passing_run_is_stored_and_exits_0() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "");
    health(
        ws.path(),
        "status",
        "{{base_url}}/status/200",
        "assertions:\n  - status: 200\n",
    );
    let (code, stdout, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.starts_with("api-health: pass in "), "{stdout}");
    assert!(!stderr.contains("git-ignored"), "{stderr}");
    let results = stored(ws.path(), "api-health");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["outcome"], "pass");
    assert_eq!(results[0]["requests"][0]["code"], 200);
    assert_eq!(results[0]["summary"]["passed"], 1);
    let state =
        fs::read_to_string(ws.path().join(".rustman/monitors/api-health/state.json")).unwrap();
    assert!(state.contains("\"last_run\""), "{state}");
    assert!(state.contains("\"compacted_at\""), "{state}");
}

#[test]
fn a_failed_assertion_is_stored_with_its_message_and_exits_1() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "");
    health(
        ws.path(),
        "status",
        "{{base_url}}/status/200",
        "assertions:\n  - status: 201\n",
    );
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(1), "{stdout}");
    assert!(
        stdout.contains("health/status: status: expected 201, got 200"),
        "{stdout}"
    );
    let results = stored(ws.path(), "api-health");
    assert_eq!(results[0]["outcome"], "fail");
    assert_eq!(
        results[0]["failures"][0],
        "health/status: status: expected 201, got 200"
    );
}

#[test]
fn an_unreachable_host_is_stored_as_error_and_exits_3() {
    let ws = workspace(&format!("http://127.0.0.1:{}", closed_port()));
    monitor(ws.path(), "api-health", "");
    health(ws.path(), "status", "{{base_url}}/status/200", "");
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(3), "{stdout}");
    assert_eq!(stored(ws.path(), "api-health")[0]["outcome"], "error");
}

#[test]
fn post_without_opt_in_is_blocked_with_exit_4() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "");
    health(ws.path(), "create", "{{base_url}}/echo", "method: POST\n");
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("health/create: blocked: method POST requires allow_mutations"),
        "{stdout}"
    );
    assert_eq!(stored(ws.path(), "api-health")[0]["outcome"], "error");
    // With the opt-in it is sent.
    monitor(ws.path(), "api-health", "allow_mutations: true\n");
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(0), "{stdout}");
}

#[test]
fn delete_is_blocked_even_with_allow_mutations() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "allow_mutations: true\n");
    health(ws.path(), "remove", "{{base_url}}/echo", "method: DELETE\n");
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("blocked: method DELETE is never allowed for monitors"),
        "{stdout}"
    );
}

/// A redirect to a host outside the environment's hosts is not followed.
#[test]
fn redirects_outside_the_environment_hosts_are_blocked() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "");
    // The same server under another name is another origin.
    let url = server.url("/echo");
    let other = if url.contains("localhost") {
        url.replace("localhost", "127.0.0.1")
    } else {
        url.replace("127.0.0.1", "localhost")
    };
    let other_host = other
        .split('/')
        .nth(2)
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .to_owned();
    health(
        ws.path(),
        "moved",
        &format!("{{{{base_url}}}}/redirect/302?to={other}"),
        "",
    );
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(4), "{stdout}");
    let blocked = format!("blocked: host not allowed (http://{other_host}:");
    assert!(stdout.contains(&blocked), "{stdout}");
}

#[test]
fn the_whole_run_deadline_stops_a_slow_run() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "api-health", "timeout: 300ms\n");
    health(ws.path(), "slow", "{{base_url}}/delay/30000", "");
    let started = std::time::Instant::now();
    let (code, stdout, _) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(3), "{stdout}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(
        stdout.contains("run exceeded its timeout of 300ms"),
        "{stdout}"
    );
    let result = &stored(ws.path(), "api-health")[0];
    assert_eq!(result["outcome"], "error");
    assert_eq!(result["message"], "run exceeded its timeout of 300ms");
}

/// Canary: a secret that reaches a failed assertion's message is redacted
/// on stdout and in the store.
#[test]
fn secrets_in_failure_messages_are_redacted() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    rustman(ws.path())
        .args(["secret", "set", "token", "--env", "prod"])
        .write_stdin(CANARY)
        .assert()
        .success();
    monitor(ws.path(), "api-health", "");
    health(
        ws.path(),
        "echo",
        "{{base_url}}/echo?k={{token}}",
        "assertions:\n  - json:\n      path: $.path\n      equals: /nope\n",
    );
    let (code, stdout, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        !stdout.contains(CANARY) && !stderr.contains(CANARY),
        "{stdout}{stderr}"
    );
    let raw = ws.path().join(".rustman/monitors/api-health/raw");
    for entry in fs::read_dir(raw).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(CANARY), "{text}");
        assert!(text.contains("«redacted:token»"), "{text}");
    }
}

#[test]
fn a_store_git_would_track_is_warned_about() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    fs::remove_file(ws.path().join(".gitignore")).unwrap();
    monitor(ws.path(), "api-health", "");
    health(ws.path(), "status", "{{base_url}}/status/200", "");
    let (code, _, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: .rustman/ is not git-ignored"),
        "{stderr}"
    );
}

#[test]
fn all_runs_every_monitor_and_exits_with_the_worst_code() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "a-ok", "");
    write(
        ws.path(),
        "monitors/b-bad.rman-monitor.yaml",
        "version: 1\nname: b-bad\ntargets: [bad/]\nenvironment: prod\nschedule: {cron: '*/5 * * * *'}\n",
    );
    health(ws.path(), "status", "{{base_url}}/status/200", "");
    write(
        ws.path(),
        "bad/status.rman.yaml",
        &request(
            "bad",
            "{{base_url}}/status/200",
            "assertions:\n  - status: 500\n",
        ),
    );
    let (code, stdout, _) = run(ws.path(), &["--all"]);
    assert_eq!(code, Some(1), "{stdout}");
    assert!(stdout.contains("a-ok: pass"), "{stdout}");
    assert!(stdout.contains("b-bad: fail"), "{stdout}");
    let (code, _, stderr) = run(ws.path(), &["nope"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("monitor 'nope' not found (available: a-ok, b-bad)"),
        "{stderr}"
    );
}

#[test]
fn a_missing_target_or_environment_exits_2() {
    let ws = workspace("http://127.0.0.1:1");
    monitor(ws.path(), "api-health", "");
    let (code, _, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("target 'health/' does not exist"),
        "{stderr}"
    );
    health(ws.path(), "status", "{{base_url}}/status/200", "");
    write(
        ws.path(),
        "monitors/api-health.rman-monitor.yaml",
        "version: 1\nname: api-health\ntargets: [health/]\nenvironment: staging\nschedule: {every: 5m}\n",
    );
    let (code, _, stderr) = run(ws.path(), &["api-health"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("environment 'staging' not found"),
        "{stderr}"
    );
}
