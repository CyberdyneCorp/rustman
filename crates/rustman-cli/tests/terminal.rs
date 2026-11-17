//! Terminal behavior, stream separation, logs, reporters on the command
//! line, `--version` and completions.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, request, rustman, valid_report, workspace, write};

const CANARY: &str = "c4n4ry-t3rm1nal-s3cr3t";

/// A workspace with a passing and a failing request against `server`.
fn two_requests(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let status = "assertions:\n  - status: 200\n";
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/echo", status),
    );
    write(
        ws.path(),
        "b.rman.yaml",
        &request("b", "{{base_url}}/status/404", status),
    );
    ws
}

fn has_escapes(bytes: &[u8]) -> bool {
    bytes.contains(&0x1b) || bytes.contains(&b'\r')
}

#[test]
fn piped_output_has_no_ansi_escapes() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev"])
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!has_escapes(&out.stdout), "{:?}", out.stdout);
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("PASS  a"));
}

#[test]
fn no_color_and_dumb_terminals_have_no_ansi_escapes() {
    let server = Server::http();
    let ws = two_requests(&server);
    for (name, value) in [("NO_COLOR", "1"), ("TERM", "dumb")] {
        let out = rustman(ws.path())
            .args(["run", "--env", "dev", "--color", "auto"])
            .env(name, value)
            .output()
            .unwrap();
        assert!(!has_escapes(&out.stdout), "{name}");
        assert!(!has_escapes(&out.stderr), "{name}");
    }
}

#[test]
fn color_always_forces_color_and_never_turns_it_off() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--color", "always"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\x1b[1m\x1b[32mPASS"), "{stdout}");
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--color", "never"])
        .output()
        .unwrap();
    assert!(!has_escapes(&out.stdout));
    rustman(ws.path())
        .args(["run", "--color", "sometimes"])
        .assert()
        .code(2);
}

#[test]
fn stdout_stays_valid_json_with_debug_logs() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json", "-vv"])
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report = valid_report(&out.stdout);
    assert_eq!(report["summary"]["total"], 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DEBUG"), "{stderr}");
    assert!(stderr.contains("GET http://"), "{stderr}");
    assert!(
        stderr.contains("< content-type: application/json"),
        "{stderr}"
    );
}

#[test]
fn without_verbose_flags_there_are_no_logs() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json"])
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json", "-v"])
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("INFO") && !stderr.contains("DEBUG"),
        "{stderr}"
    );
}

#[test]
fn rust_log_is_honored() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json"])
        .env("RUST_LOG", "rustman_http=debug")
        .output()
        .unwrap();
    valid_report(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DEBUG"), "{stderr}");
}

/// A workspace whose request sends the canary secret in a plain header and
/// the query string, to a server that echoes both back.
fn canary_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets:\n  - api_token\n",
            server.url("")
        ),
    );
    write(
        ws.path(),
        "leak.rman.yaml",
        &request(
            "leak",
            "{{base_url}}/echo?key={{api_token}}",
            "headers:\n  - name: X-Token\n    value: '{{api_token}}'\nassertions:\n  - status: 201\n",
        ),
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    ws
}

fn assert_no_canary(what: &str, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    assert!(
        !text.contains(CANARY),
        "the canary secret is in {what}:\n{text}"
    );
}

#[test]
fn the_canary_secret_never_reaches_reports_or_logs() {
    let server = Server::http();
    let ws = canary_workspace(&server);
    let out = rustman(ws.path())
        .args(["run", "--env", "dev", "-vvv", "--reporter", "json"])
        .args([
            "--reporter",
            "junit:out/junit.xml",
            "--reporter",
            "tap:out/r.tap",
        ])
        .args(["--reporter", "pretty:out/pretty.txt"])
        .env("RUST_LOG", "trace")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report = valid_report(&out.stdout);
    let url = report["requests"][0]["exchange"]["request"]["url"].as_str();
    assert!(url.unwrap().ends_with("?key=«redacted:api_token»"));
    assert_no_canary("stdout", &out.stdout);
    assert_no_canary("the logs", &out.stderr);
    assert!(String::from_utf8_lossy(&out.stderr).contains("TRACE"));
    for file in ["junit.xml", "r.tap", "pretty.txt"] {
        let bytes = fs::read(ws.path().join("out").join(file)).unwrap();
        assert!(!bytes.is_empty(), "{file}");
        assert_no_canary(file, &bytes);
    }
}

#[test]
fn junit_and_pretty_together() {
    let server = Server::http();
    let ws = two_requests(&server);
    let out = rustman(ws.path())
        .args([
            "run",
            ".",
            "--env",
            "dev",
            "--reporter",
            "junit:out/junit.xml",
        ])
        .args(["--reporter", "pretty"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL  b"));
    let xml = fs::read_to_string(ws.path().join("out/junit.xml")).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let cases: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .collect();
    assert_eq!(cases.len(), 2);
    let failures = |i: usize| {
        cases[i]
            .children()
            .filter(|n| n.has_tag_name("failure"))
            .count()
    };
    assert_eq!((failures(0), failures(1)), (0, 1));
}

#[test]
fn tap_goes_to_stdout() {
    let server = Server::http();
    let ws = two_requests(&server);
    rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "tap"])
        .assert()
        .code(1)
        .stdout(predicates::str::starts_with(
            "TAP version 14\n1..2\nok 1 - a\nnot ok 2 - b\n",
        ));
}

#[test]
fn version_prints_the_semver() {
    rustman(Path::new("."))
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("rustman {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn completions_print_a_script_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let out = rustman(Path::new("."))
            .args(["completions", shell])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{shell}");
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(script.contains("rustman"), "{shell}");
        assert!(script.contains("completions"), "{shell}");
    }
    rustman(Path::new("."))
        .args(["completions", "tcsh"])
        .assert()
        .code(2);
}

/// `bash -n` / `zsh -n` / `fish -n` on the generated scripts, for the shells
/// installed here. CI (ubuntu) installs all three and sets
/// `RUSTMAN_REQUIRE_SHELLS=1`, so none is skipped there.
#[cfg(unix)]
#[test]
fn completion_scripts_parse() {
    use std::process::Command;
    let required = std::env::var_os("RUSTMAN_REQUIRE_SHELLS").is_some();
    let dir = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish"] {
        let available = Command::new(shell).arg("-c").arg("true").output().is_ok();
        if !available {
            assert!(!required, "{shell} is not installed");
            continue;
        }
        let out = rustman(dir.path())
            .args(["completions", shell])
            .output()
            .unwrap();
        let script = dir.path().join(format!("rustman.{shell}"));
        fs::write(&script, &out.stdout).unwrap();
        let check = Command::new(shell).arg("-n").arg(&script).output().unwrap();
        assert!(
            check.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }
}
