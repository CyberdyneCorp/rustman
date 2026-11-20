//! `rustman monitor` alerts, test-alert, daemon, schedule-recipe, status
//! and history (add-scheduled-monitors sections 4-7) against in-process
//! servers: a webhook receiver and the test server.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{Server, request, rustman, write};
use rustman_monitor::store::Counts;
use rustman_monitor::{Outcome, RunResult, Store, Timestamp};
use rustman_test_support::{GraphqlHttpServer, Reply};

const TOKEN: &str = "canary-Alert-t0ken";
const HOOK: &str = "canary-Hook-path";

/// A webhook receiver answering `status`, on its own runtime.
struct Receiver {
    server: GraphqlHttpServer,
    _runtime: tokio::runtime::Runtime,
}

impl Receiver {
    fn start(status: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(GraphqlHttpServer::start(move |_| {
            Reply::raw(status, "application/json", "{}")
        }));
        Self {
            server,
            _runtime: runtime,
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.server.hits().iter().map(|h| h.body_text()).collect()
    }
}

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
        &format!("variables:\n  base_url: {base_url}\nsecrets: [token, hook, hook_down]\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn secret(ws: &Path, name: &str, value: &str) {
    rustman(ws)
        .args(["secret", "set", name, "--env", "prod"])
        .write_stdin(value)
        .assert()
        .success();
}

fn monitor(ws: &Path, extra: &str) {
    write(
        ws,
        "monitors/api-health.rman-monitor.yaml",
        &format!(
            "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {{every: 30s}}\n{extra}"
        ),
    );
}

/// A request echoing the token, whose assertion passes, or fails with
/// the token in its message.
fn echo(ws: &Path, pass: bool) {
    let assertion = if pass {
        "  - status: 200\n"
    } else {
        "  - json:\n      path: $.path\n      equals: /nope\n"
    };
    write(
        ws,
        "health/echo.rman.yaml",
        &request(
            "echo",
            "{{base_url}}/echo?k={{token}}",
            &format!("assertions:\n{assertion}"),
        ),
    );
}

fn cli(ws: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = rustman(ws).args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn assert_no_secret(text: &str) {
    assert!(!text.contains(TOKEN), "{text}");
    assert!(!text.contains(HOOK), "{text}");
}

/// A rule fires once (no repeat while it keeps firing) and recovers;
/// payloads, output and the store are redacted (canary).
#[test]
fn alerts_fire_once_then_recover_through_the_webhook() {
    let server = Server::http();
    let receiver = Receiver::start(200);
    let ws = workspace(&server.url(""));
    secret(ws.path(), "token", TOKEN);
    secret(
        ws.path(),
        "hook",
        &receiver.server.url(&format!("/hook/{HOOK}")),
    );
    monitor(
        ws.path(),
        "alerts:\n  rules:\n    - consecutive_failures: 2\n  notify:\n    - webhook: {url: \"{{hook}}\", template: slack}\n",
    );
    echo(ws.path(), false);
    let mut outputs = Vec::new();
    for _ in 0..3 {
        let (code, stdout, stderr) = cli(ws.path(), &["monitor", "run", "api-health"]);
        assert_eq!(code, Some(1), "{stdout}{stderr}");
        outputs.push(stdout.clone());
        assert_no_secret(&format!("{stdout}{stderr}"));
    }
    assert!(!outputs[0].contains("alert:"), "{}", outputs[0]);
    assert!(
        outputs[1]
            .contains("alert: [FIRING] api-health: consecutive_failures: 2\n  webhook (slack): ok"),
        "{}",
        outputs[1]
    );
    assert!(!outputs[2].contains("alert:"), "no repeat: {}", outputs[2]);
    echo(ws.path(), true);
    let (code, stdout, stderr) = cli(ws.path(), &["monitor", "run", "api-health"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("alert: [RESOLVED] api-health"), "{stdout}");
    let bodies = receiver.bodies();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(bodies[0].contains("«redacted:token»"), "{}", bodies[0]);
    assert_no_secret(&bodies[0]);
    assert!(bodies[1].contains("the outage lasted"), "{}", bodies[1]);
    let state = fs::read_to_string(ws.path().join(".rustman/monitors/api-health/state.json"));
    assert_no_secret(&state.unwrap());
    // The raw store keeps the failure messages, redacted.
    let raw_dir = ws.path().join(".rustman/monitors/api-health/raw");
    let raw: String = fs::read_dir(raw_dir)
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert!(raw.contains("«redacted:token»"), "{raw}");
    assert_no_secret(&raw);
    for args in [
        &["monitor", "status", "--json"][..],
        &["monitor", "status"],
        &["monitor", "history", "api-health", "--json"],
        &["monitor", "history", "api-health"],
    ] {
        let (code, stdout, stderr) = cli(ws.path(), args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        assert_no_secret(&format!("{stdout}{stderr}"));
    }
}

/// monitors "Verify channels": every channel gets `[TEST] api-health`,
/// the CLI prints `ok` or the error per channel, and exits 0 only when all
/// succeed.
#[test]
fn test_alert_reports_each_channel() {
    let server = Server::http();
    let ok = Receiver::start(200);
    let down = Receiver::start(500);
    let ws = workspace(&server.url(""));
    secret(ws.path(), "hook", &ok.server.url(&format!("/hook/{HOOK}")));
    secret(
        ws.path(),
        "hook_down",
        &down.server.url(&format!("/down/{HOOK}")),
    );
    monitor(
        ws.path(),
        "alerts:\n  notify:\n    - webhook: {url: \"{{hook}}\", template: slack}\n    - webhook: {url: \"{{hook_down}}\"}\n",
    );
    let (code, stdout, stderr) = cli(ws.path(), &["monitor", "test-alert", "api-health"]);
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    assert_eq!(
        stdout,
        "webhook (slack): ok\nwebhook (generic): error: HTTP 500\n"
    );
    assert_no_secret(&format!("{stdout}{stderr}"));
    assert!(
        ok.bodies()[0].contains("\"text\":\"[TEST] api-health\""),
        "{:?}",
        ok.bodies()
    );
    assert!(down.bodies()[0].contains("[TEST] api-health"));

    monitor(
        ws.path(),
        "alerts:\n  notify:\n    - webhook: {url: \"{{hook}}\", template: discord}\n",
    );
    let (code, stdout, _) = cli(ws.path(), &["monitor", "test-alert", "api-health"]);
    assert_eq!(
        (code, stdout.as_str()),
        (Some(0), "webhook (discord): ok\n")
    );

    monitor(ws.path(), "");
    let (code, _, stderr) = cli(ws.path(), &["monitor", "test-alert", "api-health"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("has no alerts.notify channels"), "{stderr}");
}

#[test]
fn schedule_recipes_print_and_write_only_when_asked() {
    let ws = workspace("http://127.0.0.1:1");
    write(
        ws.path(),
        "monitors/api-health.rman-monitor.yaml",
        "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {cron: \"*/15 * * * *\"}\n",
    );
    let before = fs::read_dir(ws.path()).unwrap().count();
    let (code, stdout, stderr) = cli(
        ws.path(),
        &[
            "monitor",
            "schedule-recipe",
            "api-health",
            "--for",
            "github-actions",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("    - cron: \"*/15 * * * *\"\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("run: rustman monitor run api-health\n"),
        "{stdout}"
    );
    assert_eq!(fs::read_dir(ws.path()).unwrap().count(), before);
    let (code, stdout, _) = cli(
        ws.path(),
        &[
            "monitor",
            "schedule-recipe",
            "api-health",
            "--for",
            "cron",
            "--write",
            "crontab.txt",
        ],
    );
    assert_eq!(code, Some(0));
    assert_eq!(stdout, "wrote crontab.txt\n");
    let crontab = fs::read_to_string(ws.path().join("crontab.txt")).unwrap();
    assert!(crontab.contains("*/15 * * * * cd "), "{crontab}");
    assert!(crontab.contains(" monitor run api-health\n"), "{crontab}");
}

fn result(at: Timestamp, outcome: Outcome) -> RunResult {
    RunResult {
        at,
        outcome,
        duration_ms: 100.0,
        summary: Counts::default(),
        requests: Vec::new(),
        failures: if outcome == Outcome::Fail {
            vec!["health/status: status: expected 200, got 503".into()]
        } else {
            Vec::new()
        },
        message: None,
    }
}

/// 288 runs every 4 minutes in the last 24 hours, 3 failed and 1 skipped,
/// plus one older failure. The oldest run is 19 hours back, far inside
/// the window, so a slow runner between seeding and `status` changes
/// nothing.
fn day_of_results(ws: &Path) {
    let store = Store::new(ws);
    let now = Timestamp::now();
    let minutes = |m: i64| minutes_before(now, m);
    for i in 0..288 {
        let outcome = match i {
            10 | 20 | 30 => Outcome::Fail,
            40 => Outcome::Skipped,
            _ => Outcome::Pass,
        };
        store
            .record("api-health", &result(minutes(4 * i + 2), outcome))
            .unwrap();
    }
    store
        .record("api-health", &result(minutes(30 * 60), Outcome::Fail))
        .unwrap();
}

/// `now` minus `m` minutes.
fn minutes_before(now: Timestamp, m: i64) -> Timestamp {
    Timestamp::from_second(now.as_second() - m * 60).unwrap()
}

/// monitors "Uptime calculation" and "Companion absent".
#[test]
fn status_shows_uptime_and_falls_back_from_viz() {
    let ws = workspace("http://127.0.0.1:1");
    monitor(ws.path(), "");
    day_of_results(ws.path());
    let (code, stdout, stderr) = cli(ws.path(), &["monitor", "status"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("api-health (prod, every 30s)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("  uptime    98.95% 24h"), "{stdout}");
    let (_, json, _) = cli(ws.path(), &["monitor", "status", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let status = &json["monitors"][0];
    assert_eq!(status["uptime"]["24h"], 98.95);
    assert_eq!(status["hourly_p95_ms"].as_array().unwrap().len(), 24);
    let empty = tempfile::tempdir().unwrap();
    let output = rustman(ws.path())
        .args(["monitor", "status", "--viz"])
        .env_remove("RUSTMAN_VIZ")
        .env("PATH", empty.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), stdout);
    assert!(
        stderr.starts_with(
            "note: the viz companion is unavailable for monitor trends (Viz companion not found"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("; showing the text status"), "{stderr}");
}

/// A stub companion: it speaks `rustman-viz/1` to `--protocol-check`,
/// answers `hello` with `views`, and records every line in `<script>.log`.
#[cfg(unix)]
fn stub_companion(dir: &Path, views: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let ack = format!(
        r#"{{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub-viz 0.1","views":[{views}]}}"#
    );
    let script = dir.join("stub-viz");
    let text = format!(
        "#!/bin/sh\n[ \"$1\" = --protocol-check ] && {{ echo rustman-viz/1; exit 0; }}\n\
         IFS= read -r hello\nprintf '%s\\n' \"$hello\" > \"$0.log\"\nprintf '%s\\n' '{ack}'\ncat >> \"$0.log\"\n"
    );
    fs::write(&script, text).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// monitors "Trend charts in the viz companion": with a companion that
/// has the view, `status --viz` opens `monitor_trend` and sends the
/// monitor's uptime gauges, latency points and heatmap, then `close`;
/// the text status is still printed and the exit code is 0.
#[cfg(unix)]
#[test]
fn status_viz_sends_the_trend_to_the_companion() {
    let ws = workspace("http://127.0.0.1:1");
    monitor(ws.path(), "");
    day_of_results(ws.path());
    let script = stub_companion(ws.path(), r#""monitor_trend""#);
    let output = rustman(ws.path())
        .args(["monitor", "status", "--viz"])
        .env("RUSTMAN_VIZ", &script)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("monitor trends opened in stub-viz 0.1"),
        "{stderr}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("  uptime    98.95% 24h"), "{stdout}");
    let log_path = format!("{}.log", script.display());
    // The stub may still be writing when rustman exits: wait for `close`.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut log = String::new();
    while !log.contains(r#""type":"close""#) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        log = fs::read_to_string(&log_path).unwrap_or_default();
    }
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["hello", "open_view", "monitor_trend", "close"]);
    assert_eq!(lines[1]["view"], "monitor_trend");
    let trend = &lines[2];
    assert_eq!(trend["monitor"], "api-health");
    assert_eq!(trend["uptime_24h"], 98.95);
    assert_eq!(trend["hour_of_day"].as_array().unwrap().len(), 24);
    let failed: u64 = trend["hour_of_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["failed"].as_u64().unwrap())
        .sum();
    assert_eq!(failed, 4, "three recent failures and the older one");
    assert!(!trend["latency"].as_array().unwrap().is_empty());

    // A companion without the view gets nothing past the handshake.
    let script = stub_companion(ws.path(), r#""run_dashboard""#);
    let output = rustman(ws.path())
        .args(["monitor", "status", "--viz"])
        .env("RUSTMAN_VIZ", &script)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "unavailable for monitor trends (the viz companion has no monitor_trend view)"
        ),
        "{stderr}"
    );
}

/// monitors "Filter failures".
#[test]
fn history_filters_failures_of_the_last_day() {
    let ws = workspace("http://127.0.0.1:1");
    monitor(ws.path(), "");
    day_of_results(ws.path());
    let args = [
        "monitor",
        "history",
        "api-health",
        "--since",
        "24h",
        "--outcome",
        "fail",
    ];
    let (code, stdout, stderr) = cli(ws.path(), &args);
    assert_eq!(code, Some(0), "{stderr}");
    let runs: Vec<&str> = stdout.lines().filter(|l| !l.starts_with(' ')).collect();
    assert_eq!(runs.len(), 3, "{stdout}");
    assert!(runs.iter().all(|l| l.contains("  fail ")), "{stdout}");
    assert!(stdout.contains("    health/status: status: expected 200, got 503"));
    let (_, all, _) = cli(
        ws.path(),
        &[
            "monitor",
            "history",
            "api-health",
            "--outcome",
            "fail",
            "--json",
        ],
    );
    assert_eq!(all.lines().count(), 4);
    let (code, _, stderr) = cli(
        ws.path(),
        &["monitor", "history", "api-health", "--outcome", "bad"],
    );
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("not one of pass, fail, error or skipped"),
        "{stderr}"
    );
}

/// A daemon process with its stderr lines on a channel.
struct Daemon {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Daemon {
    fn start(ws: &Path) -> Self {
        let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
            .args(["monitor", "daemon"])
            .current_dir(ws)
            .env("RUSTMAN_SECRET_STORE", "file")
            .env("RUSTMAN_DATA_DIR", common::test_data_dir())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Self { child, lines }
    }

    /// Wait (generously) for a line containing `text`.
    fn wait_for(&self, text: &str) -> String {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("no '{text}' from the daemon"));
            if line.contains(text) {
                return line;
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The TCP ports `pid` listens on, or `None` when no tool can tell.
fn listening(pid: u32) -> Option<Vec<String>> {
    if cfg!(windows) {
        let out = std::process::Command::new("netstat")
            .args(["-ano", "-p", "TCP"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let pid = pid.to_string();
        return Some(
            text.lines()
                .filter(|l| {
                    l.contains("LISTENING") && l.split_whitespace().last() == Some(pid.as_str())
                })
                .map(str::to_owned)
                .collect(),
        );
    }
    let out = std::process::Command::new("lsof")
        .args(["-a", "-nP", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    Some(text.lines().skip(1).map(str::to_owned).collect())
}

/// monitors "Local daemon": runs on schedule and logs to stderr, binds no
/// port, refuses a second daemon, and stops cleanly on SIGTERM.
#[test]
fn the_daemon_runs_monitors_without_a_port_and_only_once() {
    let server = Server::http();
    let ws = workspace(&server.url(""));
    monitor(ws.path(), "");
    write(
        ws.path(),
        "health/status.rman.yaml",
        &request(
            "status",
            "{{base_url}}/status/200",
            "assertions:\n  - status: 200\n",
        ),
    );
    let daemon = Daemon::start(ws.path());
    daemon.wait_for("monitor daemon started: 1 monitor(s), no listening port");
    let line = daemon.wait_for("api-health:");
    assert!(line.contains("api-health: pass in "), "{line}");
    if let Some(ports) = listening(daemon.child.id()) {
        assert!(ports.is_empty(), "{ports:?}");
    }
    let (code, _, stderr) = cli(ws.path(), &["monitor", "daemon"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "monitor daemon already running (pid {})",
            daemon.child.id()
        )),
        "{stderr}"
    );
    #[cfg(unix)]
    {
        let mut daemon = daemon;
        let pid = daemon.child.id().to_string();
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap();
        daemon.wait_for("monitor daemon stopped");
        let status = daemon.child.wait().unwrap();
        assert_eq!(status.code(), Some(0));
        assert!(!ws.path().join(".rustman/monitors/daemon.pid").exists());
    }
}
