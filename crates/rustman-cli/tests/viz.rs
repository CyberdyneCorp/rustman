//! The viz companion from the CLI (add-raven-visualizer tasks 2.3–2.6):
//! `--viz` on `run` and `explore run` streams to a stub companion that
//! records what it reads, without one the command continues with a
//! warning and an unchanged exit code, and `rustman viz <REPORT>` opens a
//! saved report or exits 2 for a file that is not one. Every recorded
//! stream is checked for the canary secret the requests send.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Server, request, rustman, workspace, write};

const CANARY: &str = "viz-canary-7f3a91";

const ACK: &str = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub-viz 0.1","views":["run_dashboard","exploration_dashboard","api_graph","response_inspector"]}"#;

/// `rustman` that can find no companion: no override and a `PATH` with
/// nothing but an empty directory (the test binary's own directory holds
/// no `rustman-viz`).
fn without_companion(dir: &Path, empty: &Path) -> assert_cmd::Command {
    let mut cmd = rustman(dir);
    cmd.env_remove("RUSTMAN_VIZ").env("PATH", empty);
    cmd
}

/// A workspace whose `dev` environment holds the secret `token` (the
/// canary), with three requests that send it in the query.
fn secret_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.url("")
        ),
    );
    rustman(ws.path())
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let assert = "assertions:\n  - status: 200\n";
    for name in ["a", "b", "c"] {
        write(
            ws.path(),
            &format!("api/{name}.rman.yaml"),
            &request(name, "{{base_url}}/echo?t={{token}}", assert),
        );
    }
    ws
}

/// A stub companion: it speaks `rustman-viz/1` to `--protocol-check`,
/// answers `hello` with `reply`, then runs `rest` (sh). The default rest
/// records every line it reads in `<script>.log`.
#[cfg(unix)]
fn stub(dir: &Path, reply: &str, rest: Option<&str>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("stub-viz");
    let rest = rest.unwrap_or("cat >> \"$0.log\"");
    let text = format!(
        "#!/bin/sh\n[ \"$1\" = --protocol-check ] && {{ echo rustman-viz/1; exit 0; }}\n\
         IFS= read -r hello\nprintf '%s\\n' \"$hello\" > \"$0.log\"\nprintf '%s\\n' '{reply}'\n{rest}\n"
    );
    fs::write(&script, text).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The stub's log once it holds `until`.
#[cfg(unix)]
fn log_of(script: &Path, until: &str) -> String {
    let log = PathBuf::from(format!("{}.log", script.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        if text.contains(until) || Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The `type` of every line of a recorded stream.
#[cfg(unix)]
fn kinds(log: &str) -> Vec<String> {
    log.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["type"].as_str().unwrap().to_owned()
        })
        .collect()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn run_viz_without_a_companion_warns_and_keeps_the_exit_code() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    let empty = tempfile::tempdir().unwrap();
    let output = without_companion(ws.path(), empty.path())
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(
        err.contains("Viz companion not found; continuing without visuals"),
        "{err}"
    );
    assert!(err.contains("--report html="), "{err}");

    // A failing run keeps its own exit code too.
    write(
        ws.path(),
        "api/d.rman.yaml",
        &request("d", "{{base_url}}/echo", "assertions:\n  - status: 418\n"),
    );
    let output = without_companion(ws.path(), empty.path())
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn an_invalid_override_names_the_path() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    let nope = ws.path().join("nope").join("rustman-viz");
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &nope)
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(err.contains(&nope.display().to_string()), "{err}");
}

#[test]
fn viz_rejects_a_file_that_is_not_a_report() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "notes.txt", "just some notes\n");
    write(dir.path(), "data.json", "{\"hello\": 1}\n");
    for file in ["notes.txt", "data.json", "missing.json"] {
        let output = rustman(dir.path()).args(["viz", file]).output().unwrap();
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{file}: {err}");
        assert!(
            err.contains(&format!("{file} is not a Rustman report")),
            "{err}"
        );
    }
}

#[test]
fn viz_without_a_companion_offers_the_html_report() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    rustman(ws.path())
        .args([
            "run",
            "api",
            "--env",
            "dev",
            "--reporter",
            "json:out/run.json",
        ])
        .assert()
        .success();
    let empty = tempfile::tempdir().unwrap();
    let output = without_companion(ws.path(), empty.path())
        .args(["viz", "out/run.json"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(2), "{err}");
    assert!(err.contains("Viz companion not found"), "{err}");
    assert!(
        err.contains("rustman report render out/run.json --format html"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn run_viz_streams_the_run_redacted() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    let script = stub(ws.path(), ACK, None);
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(!err.contains("warning"), "{err}");
    let log = log_of(&script, r#""type":"close""#);
    assert_eq!(
        kinds(&log),
        [
            "hello",
            "open_view",
            "run_started",
            "exchange_completed",
            "exchange_completed",
            "exchange_completed",
            "run_finished",
            "close"
        ],
        "{log}"
    );
    assert!(log.contains(r#""view":"run_dashboard""#), "{log}");
    assert!(log.contains(r#""total":3"#), "{log}");
    assert!(log.contains(r#""environment":"dev""#), "{log}");
    assert!(log.contains(r#""request":"api/a""#), "{log}");
    assert!(log.contains(r#""outcome":"passed""#), "{log}");
    assert!(log.contains(r#""state":"completed""#), "{log}");
    assert!(log.contains("redacted:token"), "{log}");
    assert!(
        !log.contains(CANARY),
        "the secret reached the companion: {log}"
    );
    // The companion's stderr goes to the workspace's state directory.
    assert!(ws.path().join(".rustman/viz.log").exists());
}

#[cfg(unix)]
#[test]
fn a_companion_that_exits_mid_run_does_not_change_the_run() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    // Crashes right after the handshake, long before the run ends.
    let script = stub(ws.path(), ACK, Some("exit 1"));
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["run", "api", "--env", "dev", "--viz"])
        .args(["--reporter", "json:out/run.json"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    let report = fs::read_to_string(ws.path().join("out/run.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["summary"]["passed"], 3, "{report}");
    assert_eq!(err.matches("Viz companion exited").count(), 1, "{err}");
}

#[cfg(unix)]
#[test]
fn rejected_and_graphicless_companions_are_explained() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    let reject = r#"{"type":"hello_reject","supported":["rustman-viz/2"]}"#;
    let script = stub(ws.path(), reject, None);
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(
        err.contains("Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1"),
        "{err}"
    );
    assert!(err.contains("continuing without visuals"), "{err}");

    let dir = ws.path().join("vulkan");
    fs::create_dir_all(&dir).unwrap();
    let vulkan = r#"{"type":"error","code":"vulkan_unavailable","message":"no Vulkan loader"}"#;
    let script = stub(&dir, vulkan, Some("exit 3"));
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["run", "api", "--env", "dev", "--viz"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(
        err.contains("Graphics unavailable (Vulkan); continuing without visuals"),
        "{err}"
    );
    assert!(err.contains("--report html="), "{err}");
}

#[cfg(unix)]
#[test]
fn viz_opens_saved_reports_in_the_matching_view() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    rustman(ws.path())
        .args([
            "run",
            "api",
            "--env",
            "dev",
            "--reporter",
            "json:out/run.json",
        ])
        .assert()
        .success();
    let script = stub(ws.path(), ACK, None);
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["viz", "out/run.json"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{err}");
    assert!(err.contains("Opened out/run.json in stub-viz 0.1"), "{err}");
    let log = log_of(&script, r#""type":"close""#);
    assert_eq!(
        kinds(&log),
        ["hello", "open_view", "load_report", "close"],
        "{log}"
    );
    assert!(log.contains(r#""kind":"run""#), "{log}");
    assert!(log.contains(r#""view":"run_dashboard""#), "{log}");
    let full = fs::canonicalize(ws.path().join("out/run.json")).unwrap();
    let path = serde_json::to_string(&full.display().to_string()).unwrap();
    assert!(log.contains(&format!(r#""path":{path}"#)), "{log}");

    // A graph needs an exploration.
    let output = rustman(ws.path())
        .env("RUSTMAN_VIZ", &script)
        .args(["viz", "out/run.json", "--view", "graph"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(2), "{err}");
    assert!(
        err.contains("--view graph needs an exploration report"),
        "{err}"
    );
}

#[cfg(unix)]
mod explore {
    use rustman_test_support::bad_api::{BadApi, OPENAPI, TOKEN_A};

    use super::*;

    #[test]
    fn explore_viz_streams_probes_and_findings() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let api = runtime.block_on(BadApi::start());
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
        rustman(dir.path())
            .args(["secret", "set", "bearerAuth_token", "--env", "dev"])
            .write_stdin(TOKEN_A)
            .assert()
            .success();
        let script = stub(dir.path(), ACK, None);
        let target = api.url("");
        let output = rustman(dir.path())
            .env("RUSTMAN_VIZ", &script)
            .args(["explore", "run", "--spec", "api.yaml", "--target", &target])
            .args(["--seed", "7", "--rate", "1000", "--env", "dev"])
            .args([
                "--probes",
                "conformance",
                "--operations",
                "getUser",
                "--viz",
            ])
            .output()
            .unwrap();
        let err = stderr(&output);
        // One medium finding, below the default --fail-on high.
        assert_eq!(output.status.code(), Some(0), "{err}");
        let log = log_of(&script, r#""type":"close""#);
        let kinds = kinds(&log);
        assert_eq!(
            kinds[..3],
            ["hello", "open_view", "exploration_started"],
            "{log}"
        );
        assert!(kinds.contains(&"probe_completed".to_owned()), "{log}");
        assert_eq!(
            kinds.iter().filter(|k| *k == "finding").count(),
            1,
            "one finding per rule and operation: {log}"
        );
        assert_eq!(
            kinds[kinds.len() - 2..],
            ["exploration_finished", "close"],
            "{log}"
        );
        assert!(log.contains(r#""view":"exploration_dashboard""#), "{log}");
        assert!(log.contains(r#""severity":"medium""#), "{log}");
        assert!(log.contains(r#""operation":"GET /users/{id}""#), "{log}");
        assert!(log.contains(r#""state":"completed""#), "{log}");
        assert!(
            !log.contains(TOKEN_A),
            "the token reached the companion: {log}"
        );
        drop(api);
    }
}
