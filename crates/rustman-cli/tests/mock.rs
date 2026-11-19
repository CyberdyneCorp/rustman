//! `rustman mock serve` (spec: Serve command and bind address, Loopback by
//! default, Document loading, Mock exit codes and shutdown).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::rustman;

fn pets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rustman-mock/tests/fixtures/pets.yaml")
}

fn pets_arg() -> String {
    pets().to_string_lossy().into_owned()
}

#[test]
fn remote_bind_refused() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["mock", "serve", &pets_arg(), "--host", "0.0.0.0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: mock: binding 0.0.0.0 exposes the mock to the network; pass --allow-remote"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn port_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = rustman(dir.path())
        .args(["mock", "serve", &pets_arg(), "--port", &port])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        format!("error: mock: cannot bind 127.0.0.1:{port}: address in use")
    );
}

#[test]
fn invalid_document() {
    let dir = tempfile::tempdir().unwrap();
    let text = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /a:\n    get:\n      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              schema: {$ref: '#/components/schemas/Nope'}\n";
    std::fs::write(dir.path().join("api.yaml"), text).unwrap();
    let output = rustman(dir.path())
        .args(["mock", "serve", "api.yaml", "--port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(
            "error: /paths/~1a/get/responses/200/content/application~1json/schema/$ref: unresolved reference '#/components/schemas/Nope'"
        ),
        "{stderr}"
    );
}

/// `rustman mock serve <pets> <args>` in `dir`, stdout and stderr piped.
fn spawn_serve(dir: &Path, args: &[&str]) -> Child {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV")
        .args(["mock", "serve", &pets_arg()])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The first line of the child's stdout (killed if it never comes).
fn first_line(child: &mut Child) -> String {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| {
            let _ = child.kill();
            panic!("no banner");
        })
}

/// The `127.0.0.1:<port>` of the banner.
fn banner_addr(banner: &str) -> String {
    banner
        .trim_end()
        .strip_prefix("mock: serving 8 operations on http://")
        .unwrap_or_else(|| panic!("unexpected banner {banner:?}"))
        .to_owned()
}

#[test]
fn default_start_prints_the_banner_and_serves() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_serve(dir.path(), &["--port", "0"]);
    let addr = banner_addr(&first_line(&mut child));
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    let mut stream = TcpStream::connect(&addr).unwrap();
    stream
        .write_all(b"GET /users/me HTTP/1.1\r\nHost: m\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(r#"{"id":"me"}"#), "{response}");
    child.kill().unwrap();
    let _ = child.wait();
}

#[cfg(unix)]
#[test]
fn graceful_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_serve(dir.path(), &["--port", "0"]);
    let addr = banner_addr(&first_line(&mut child));
    let mut stream = TcpStream::connect(&addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    // In flight: the head is sent, the body only after Ctrl+C.
    stream
        .write_all(b"POST /users HTTP/1.1\r\nHost: m\r\nContent-Length: 2\r\n\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "exited before the request finished"
    );
    stream.write_all(b"{}").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        assert!(std::time::Instant::now() < deadline, "did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(code, Some(130));
}

/// A workspace in `dir` with a mock file holding `mock`.
fn workspace_with_mock(dir: &Path, settings: &str, mock: &str) {
    let toml = format!("[workspace]\nname = \"w\"\nversion = 1\n{settings}");
    std::fs::write(dir.join("rustman.toml"), toml).unwrap();
    std::fs::create_dir_all(dir.join("mocks")).unwrap();
    std::fs::write(dir.join("mocks").join("health.rman-mock.yaml"), mock).unwrap();
}

/// One request on its own connection; the whole response.
fn exchange(addr: &str, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
}

#[test]
fn overrides_validation_and_stateful_flags() {
    let dir = tempfile::tempdir().unwrap();
    workspace_with_mock(
        dir.path(),
        "",
        "overrides:\n  - match: {method: GET, path: /health}\n    respond: {body: {ok: true}}\n",
    );
    let mut child = spawn_serve(dir.path(), &["--port", "0", "--stateful", "--no-validate"]);
    let addr = banner_addr(&first_line(&mut child));
    let health = exchange(
        &addr,
        "GET /health HTTP/1.1\r\nHost: m\r\nConnection: close\r\n\r\n",
    );
    assert!(health.ends_with(r#"{"ok":true}"#), "{health}");
    let body = r#"{"name":5}"#;
    let create = format!(
        "POST /pets HTTP/1.1\r\nHost: m\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let created = exchange(&addr, &create);
    assert!(created.starts_with("HTTP/1.1 201"), "{created}");
    assert!(created.ends_with(r#"{"name":5,"id":1}"#), "{created}");
    let read = exchange(
        &addr,
        "GET /pets/1 HTTP/1.1\r\nHost: m\r\nConnection: close\r\n\r\n",
    );
    assert!(read.ends_with(r#"{"name":5,"id":1}"#), "{read}");
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn an_invalid_mock_file_is_a_startup_error() {
    let dir = tempfile::tempdir().unwrap();
    workspace_with_mock(
        dir.path(),
        "",
        "overrides:\n  - match: {path: /a}\n    delay_ms: 70000\n",
    );
    let output = rustman(dir.path())
        .args(["mock", "serve", &pets_arg(), "--port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("health.rman-mock.yaml"), "{stderr}");
    assert!(
        stderr.contains("override 1: delay_ms 70000 exceeds 60000"),
        "{stderr}"
    );
}

#[test]
fn security_values_must_be_secret_references() {
    let dir = tempfile::tempdir().unwrap();
    workspace_with_mock(
        dir.path(),
        "[mock.security]\nbearerAuth = \"hunter2\"\n",
        "overrides: []\n",
    );
    let output = rustman(dir.path())
        .args([
            "mock",
            "serve",
            &pets_arg(),
            "--port",
            "0",
            "--enforce-security",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("[mock.security] bearerAuth: secret must be a {{variable}} reference"),
        "{stderr}"
    );
    assert!(!stderr.contains("hunter2"), "{stderr}");
}

/// The first line of `reader` (the child is killed if it never comes).
fn line_of(
    reader: impl Read + Send + 'static,
    child: &mut Child,
) -> (String, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let first = rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| {
            let _ = child.kill();
            panic!("no output");
        });
    (first, rx)
}

#[test]
fn json_lines_and_redacted_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_serve(dir.path(), &["--port", "0", "--jsonl"]);
    let stderr = child.stderr.take().unwrap();
    let (banner, _) = line_of(stderr, &mut child);
    let addr = banner_addr(&banner);
    let stdout = child.stdout.take().unwrap();
    let pets = exchange(
        &addr,
        "GET /pets/1?api_key=qcanary99 HTTP/1.1\r\nHost: m\r\nAuthorization: Bearer abc\r\nX-Api-Key: canary-apikey-123456\r\nConnection: close\r\n\r\n",
    );
    assert!(pets.starts_with("HTTP/1.1 200"), "{pets}");
    let (line, _) = line_of(stdout, &mut child);
    child.kill().unwrap();
    let _ = child.wait();
    let entry: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(entry["status"], 200, "{line}");
    assert_eq!(entry["source"], "example", "{line}");
    assert!(line.contains("«redacted:Authorization»"), "{line}");
    assert!(!line.contains("Bearer abc"), "{line}");
    assert!(line.contains("«redacted:x-api-key»"), "{line}");
    assert!(line.contains("api_key=«redacted:api_key»"), "{line}");
    assert!(!line.contains("canary-apikey-123456"), "{line}");
    assert!(!line.contains("qcanary99"), "{line}");
}

#[test]
fn exchanges_in_history() {
    let dir = tempfile::tempdir().unwrap();
    workspace_with_mock(dir.path(), "", "overrides: []\n");
    let mut child = spawn_serve(dir.path(), &["--port", "0", "--history"]);
    let addr = banner_addr(&first_line(&mut child));
    for path in ["/pets/1", "/users/me", "/nope"] {
        exchange(
            &addr,
            &format!("GET {path} HTTP/1.1\r\nHost: m\r\nConnection: close\r\n\r\n"),
        );
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let rows = loop {
        let output = rustman(dir.path())
            .args(["history", "list", "--json"])
            .output()
            .unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        if rows.len() >= 3 || std::time::Instant::now() > deadline {
            break rows;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    child.kill().unwrap();
    let _ = child.wait();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows.iter().all(|r| r["request"] == "mock"), "{rows:?}");
    let statuses: Vec<_> = rows.iter().map(|r| r["status"].as_u64().unwrap()).collect();
    assert!(
        statuses.contains(&404) && statuses.contains(&200),
        "{statuses:?}"
    );
}

#[test]
fn record_needs_proxy_unmatched_and_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["mock", "serve", &pets_arg(), "--record", "recorded"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--proxy-unmatched"), "{stderr}");
    let output = rustman(dir.path())
        .args([
            "mock",
            "serve",
            &pets_arg(),
            "--port",
            "0",
            "--proxy-unmatched",
            "http://127.0.0.1:9",
            "--record",
            "recorded",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--record needs a workspace"), "{stderr}");
    let toml = "[workspace]\nname = \"w\"\nversion = 1\n";
    std::fs::write(dir.path().join("rustman.toml"), toml).unwrap();
    let outside = rustman(dir.path())
        .args([
            "mock",
            "serve",
            &pets_arg(),
            "--port",
            "0",
            "--proxy-unmatched",
            "http://127.0.0.1:9",
            "--record",
            "../outside",
        ])
        .output()
        .unwrap();
    assert_eq!(outside.status.code(), Some(2));
    let stderr = String::from_utf8(outside.stderr).unwrap();
    assert!(
        stderr.contains("--record \"../outside\": the directory must be inside the workspace"),
        "{stderr}"
    );
    assert!(!stderr.contains("--into"), "{stderr}");
    let bad = rustman(dir.path())
        .args(["mock", "serve", &pets_arg(), "--proxy-unmatched", "ftp://x"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
}

/// Task 7.3: the explorer's conformance probes against a mock of the same
/// document find nothing (the CI self-check runs the same commands).
#[test]
fn explorer_self_check() {
    self_check("petstore.yaml", 5);
}

/// Synthesized strings for patterns and required members nested past the
/// depth cap conform too.
#[test]
fn explorer_self_check_of_patterns_and_deep_required_members() {
    self_check("patterns.yaml", 2);
}

/// Serve the fixture `name` (with `operations` operations) and run the
/// explorer's conformance probes against it: no findings.
fn self_check(name: &str, operations: usize) {
    let dir = tempfile::tempdir().unwrap();
    let toml = "[workspace]\nname = \"selfcheck\"\nversion = 1\n";
    std::fs::write(dir.path().join("rustman.toml"), toml).unwrap();
    std::fs::create_dir(dir.path().join("environments")).unwrap();
    std::fs::write(
        dir.path().join("environments").join("ci.yaml"),
        "variables:\n  bearerAuth_token: mock-token\n  apiKey_key: mock-key\n",
    )
    .unwrap();
    let petstore = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    let petstore = petstore.to_string_lossy().into_owned();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .args(["mock", "serve", &petstore, "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let banner = first_line(&mut child);
    let addr = banner
        .trim_end()
        .strip_prefix(&format!("mock: serving {operations} operations on "))
        .unwrap_or_else(|| panic!("{banner}"))
        .to_owned();
    let output = rustman(dir.path())
        .args([
            "explore",
            "run",
            "--spec",
            &petstore,
            "--target",
            &addr,
            "--probes",
            "conformance",
            "--fail-on",
            "info",
            "--env",
            "ci",
            "--allow-destructive",
        ])
        .output()
        .unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0), "{text}");
    let probes = format!("probes: {operations} ({operations} passed, 0 finding");
    assert!(text.contains(&probes), "{text}");
    assert!(
        text.contains("findings: 0 critical, 0 high, 0 medium, 0 low, 0 info"),
        "{text}"
    );
}
