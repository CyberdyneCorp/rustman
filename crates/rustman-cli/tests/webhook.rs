//! `rustman webhook listen` (spec: Listener lifecycle and bind address,
//! Non-loopback bind warning, Event log, CLI streaming output).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::rustman;

#[test]
fn a_network_address_needs_allow_remote() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["webhook", "listen", "--host", "0.0.0.0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: webhook: binding 0.0.0.0 exposes the listener to the network; pass --allow-remote"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn a_port_in_use_is_reported_and_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = rustman(dir.path())
        .args(["webhook", "listen", "--port", &port])
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        format!("error: webhook: cannot bind 127.0.0.1:{port}: address in use")
    );
}

/// Lines of the child's stdout, read on a thread.
fn lines(child: &mut Child) -> mpsc::Receiver<String> {
    reader_lines(child.stdout.take().unwrap())
}

/// Lines of `pipe`, read on a thread.
fn reader_lines(pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// `rustman webhook listen <args>` in `dir`, stdout and stderr piped.
fn spawn_listen(dir: &std::path::Path, args: &[&str]) -> Child {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV")
        .args(["webhook", "listen"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The `127.0.0.1:<port>` of a `Listening on ...` banner.
fn banner_addr(banner: &str) -> String {
    banner
        .strip_prefix("Listening on http://")
        .unwrap_or_else(|| panic!("unexpected banner {banner:?}"))
        .to_owned()
}

/// Send raw bytes and read the reply until the server closes.
fn exchange(addr: &str, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    String::from_utf8_lossy(&response).into_owned()
}

/// The exit code once the child exits on its own (killed after 30 s).
fn exit_code(child: &mut Child) -> Option<i32> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    panic!("rustman webhook listen did not exit");
}

#[test]
fn jsonl_prints_one_flushed_object_per_event_and_max_captures_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_listen(
        dir.path(),
        &["--port", "0", "--jsonl", "--max-captures", "1"],
    );
    let err = reader_lines(child.stderr.take().unwrap());
    let out = lines(&mut child);
    let addr = banner_addr(&next(&err));

    let mut request =
        b"POST /x?y=1 HTTP/1.1\r\nHost: h\r\nX-A: 1\r\nX-A: 2\r\nContent-Length: 3\r\n\r\n"
            .to_vec();
    request.extend_from_slice(&[0xff, 0xfe, 0x00]);
    let response = exchange(&addr, &request);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // Printed while the process still runs: flushed per event.
    let line = next(&out);
    let event: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(event["type"], "capture");
    assert_eq!(event["id"], 1);
    assert_eq!(event["path"], "/x");
    assert_eq!(event["query"], "y=1");
    assert_eq!(event["method"], "POST");
    assert_eq!(event["status"], 200);
    assert_eq!(event["signature"], serde_json::Value::Null);
    assert_eq!(event["body_base64"], "//4A");
    assert!(event["ts"].as_str().unwrap().contains('T'));
    let headers: Vec<_> = event["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["name"] == "X-A")
        .map(|h| h["value"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(headers, ["1", "2"]);

    assert_eq!(exit_code(&mut child), Some(0));
    assert!(
        out.recv_timeout(Duration::from_secs(5)).is_err(),
        "only one line"
    );
}

#[test]
fn a_closed_stdout_stops_at_once_without_waiting_for_replies() {
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"w\"\nversion = 1\n",
    );
    common::write(
        dir.path(),
        "webhook/mocks.yaml",
        "mocks:\n- {path: /slow, delay_ms: 60000}\n",
    );
    let mut child = spawn_listen(dir.path(), &["--port", "0", "--jsonl"]);
    let err = reader_lines(child.stderr.take().unwrap());
    let addr = banner_addr(&next(&err));
    drop(child.stdout.take());
    // Each reply waits a minute. A write fails on the closed output, and the
    // next event after that ends the process before any reply is sent.
    std::thread::spawn(move || {
        for _ in 0..50 {
            let Ok(mut stream) = TcpStream::connect(&addr) else {
                return;
            };
            let _ = stream.write_all(b"GET /slow HTTP/1.1\r\nHost: h\r\n\r\n");
            std::thread::spawn(move || stream.read_to_end(&mut Vec::new()));
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    assert_eq!(exit_code(&mut child), Some(0));
}

#[test]
fn max_captures_waits_for_the_mocked_reply_and_prints_its_match() {
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"w\"\nversion = 1\n",
    );
    common::write(
        dir.path(),
        "webhook/mocks.yaml",
        "mocks:\n- {path: /slow, status: 503, delay_ms: 300, body: later}\n",
    );
    let mut child = spawn_listen(dir.path(), &["--port", "0", "--max-captures", "1"]);
    let out = lines(&mut child);
    let addr = banner_addr(&next(&out));
    let response = exchange(&addr, b"GET /slow HTTP/1.1\r\nHost: h\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    assert!(response.ends_with("later"), "{response}");
    assert!(next(&out).starts_with("#1 GET /slow from "));
    assert_eq!(next(&out), "#1 matched mock GET /slow -> 503 after 300 ms");
    assert_eq!(exit_code(&mut child), Some(0));
}

#[test]
fn max_body_and_the_port_from_rustman_toml() {
    let dir = tempfile::tempdir().unwrap();
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    common::write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"w\"\nversion = 1\n\n[webhook]\nport = {port}\n"),
    );
    let mut child = spawn_listen(dir.path(), &["--max-body", "4"]);
    let out = lines(&mut child);
    assert_eq!(next(&out), format!("Listening on http://127.0.0.1:{port}"));
    let addr = format!("127.0.0.1:{port}");
    let response = exchange(
        &addr,
        b"POST /big HTTP/1.1\r\nHost: h\r\nContent-Length: 10\r\n\r\n0123456789",
    );
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    let line = next(&out);
    assert!(
        line.ends_with("rejected 413 body larger than 4 bytes"),
        "{line}"
    );
    child.kill().unwrap();
    let _ = child.wait();
}

fn next(lines: &mpsc::Receiver<String>) -> String {
    lines
        .recv_timeout(Duration::from_secs(20))
        .expect("no output line in time")
}

#[test]
fn listens_on_a_free_port_answers_with_mocks_and_prints_events() {
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"w\"\nversion = 1\n",
    );
    common::write(
        dir.path(),
        "webhook/mocks.yaml",
        "mocks:\n- {method: POST, path: '/hooks/*', status: 202}\n",
    );
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir.path())
        .args(["webhook", "listen", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = lines(&mut child);
    let banner = next(&out);
    let addr = banner
        .strip_prefix("Listening on http://127.0.0.1:")
        .map(|port| format!("127.0.0.1:{port}"))
        .unwrap_or_else(|| panic!("unexpected banner {banner:?}"));

    let mut stream = TcpStream::connect(&addr).unwrap();
    stream
        .write_all(b"POST /hooks/a?x=1 HTTP/1.1\r\nHost: h\r\nContent-Length: 2\r\n\r\n{}")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 202"), "{response}");

    let local = stream.local_addr().unwrap();
    assert_eq!(
        next(&out),
        format!("#1 POST /hooks/a?x=1 from {local} (2 B) -> 202")
    );
    assert_eq!(next(&out), "#1 matched mock POST /hooks/* -> 202");
    child.kill().unwrap();
    let _ = child.wait();
}

/// HMAC-SHA256 of `body` under `key`, as GitHub's `sha256=<hex>`.
fn github_signature(key: &[u8], body: &[u8]) -> String {
    let mac = rustman_webhook::signature::Algorithm::Sha256.hmac(key, &[body]);
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

#[test]
fn verifies_signatures_with_environment_secrets_and_redacts_them() {
    const SECRET: &str = "canary-gh-41d07b";
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"w\"\nversion = 1\n\n[[webhook.verifiers]]\npath = \"/hooks/github\"\ntype = \"github\"\nsecret = \"{{gh_hook}}\"\n",
    );
    common::write(
        dir.path(),
        "environments/local.yaml",
        "secrets: [gh_hook]\n",
    );
    common::write(dir.path(), ".gitignore", ".rustman/\n");
    rustman(dir.path())
        .args(["secret", "set", "gh_hook", "--env", "local"])
        .write_stdin(SECRET)
        .assert()
        .success();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV")
        .args(["webhook", "listen", "--port", "0", "--env", "local"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = lines(&mut child);
    let banner = next(&out);
    let port = banner
        .strip_prefix("Listening on http://127.0.0.1:")
        .unwrap_or_else(|| panic!("unexpected banner {banner:?}"));

    let body = br#"{"zen":"ok"}"#;
    let request = format!(
        "POST /hooks/github?echo={SECRET} HTTP/1.1\r\nHost: h\r\nX-Hub-Signature-256: {}\r\nContent-Length: {}\r\n\r\n",
        github_signature(SECRET.as_bytes(), body),
        body.len()
    );
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let line = next(&out);
    let local = stream.local_addr().unwrap();
    assert_eq!(
        line,
        format!(
            "#1 POST /hooks/github?echo=«redacted:gh_hook» from {local} (12 B) -> 200 signature: valid"
        )
    );
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn a_stdout_nobody_reads_never_stalls_replies_or_ctrl_c() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_listen(dir.path(), &["--port", "0", "--jsonl"]);
    let err = reader_lines(child.stderr.take().unwrap());
    // Held open and never read: the pipe fills after a few events.
    let _stdout = child.stdout.take().unwrap();
    let addr = banner_addr(&next(&err));

    let body = "x".repeat(60 * 1024);
    let request = format!(
        "POST /big HTTP/1.1\r\nHost: h\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    for n in 0..12 {
        let mut stream = TcpStream::connect(&addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        let response = String::from_utf8_lossy(&response);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "request {n}: {response:?}"
        );
    }

    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(exit_code(&mut child), Some(130));
    }
    #[cfg(not(unix))]
    {
        child.kill().unwrap();
        let _ = child.wait();
    }
}
