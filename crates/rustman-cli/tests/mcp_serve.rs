//! `rustman mcp serve` (agent-mcp-interface tasks 1.1-1.3): the stdio
//! handshake, stdout carrying only JSON-RPC frames, the missing workspace,
//! and both `--http` bind cases. Nothing leaves the loopback interface.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use common::{test_data_dir, write};
use serde_json::{Value, json};

/// A workspace with one request and a `dev` environment.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        "variables:\n  base_url: http://127.0.0.1:9\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

/// `rustman <args>` in `dir`, with piped stdio and the file secret store.
fn spawn(dir: &std::path::Path, args: &[&str], envs: &[(&str, &str)]) -> Child {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("rustman"));
    cmd.current_dir(dir)
        .args(args)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        cmd.env(name, value);
    }
    cmd.spawn().unwrap()
}

fn frame(value: &Value) -> String {
    format!("{value}\n")
}

/// Write `requests`, close stdin, and return every stdout line and stderr.
fn session(requests: &[Value], envs: &[(&str, &str)]) -> (Vec<String>, String, Option<i32>) {
    let ws = workspace();
    let mut child = spawn(ws.path(), &["mcp", "serve"], envs);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // Drained on its own thread: trace logs can outgrow the pipe buffer.
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr_pipe.read_to_string(&mut text);
        text
    });
    let mut lines = Vec::new();
    // Answer by answer, so the server is never shut down mid-request.
    for request in requests {
        stdin.write_all(frame(request).as_bytes()).unwrap();
        stdin.flush().unwrap();
        if request.get("id").is_some() {
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            lines.push(line);
        }
    }
    drop(stdin);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    lines.extend(rest.lines().map(|l| format!("{l}\n")));
    let status = child.wait().unwrap();
    let stderr = stderr_reader.join().unwrap();
    (lines, stderr, status.code())
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "cli-test", "version": "0"}
        }
    })
}

#[test]
fn an_agent_connects_over_stdio() {
    let requests = [
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    ];
    let (lines, stderr, code) = session(&requests, &[]);
    assert_eq!(code, Some(0), "{stderr}");
    let init: Value = serde_json::from_str(&lines[0]).unwrap();
    let result = &init["result"];
    assert_eq!(result["serverInfo"]["name"], "rustman");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    for capability in ["tools", "resources", "prompts", "logging"] {
        assert!(
            result["capabilities"].get(capability).is_some(),
            "{capability}: {result}"
        );
    }
    let tools: Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(tools["id"], 3);
    assert!(tools["result"]["tools"].is_array(), "{tools}");
}

#[test]
fn stdout_carries_only_json_rpc_frames_while_logs_go_to_stderr() {
    let requests = [
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "prompts/list"}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "no/such/method"}),
    ];
    let (lines, stderr, code) = session(&requests, &[("RUST_LOG", "trace")]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(lines.len(), 5, "{lines:?}");
    for line in &lines {
        assert!(line.ends_with('\n'));
        let frame: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(frame["jsonrpc"], "2.0", "{line}");
        assert!(frame.get("result").is_some() || frame.get("error").is_some());
    }
    // The trace logs were written, to stderr.
    assert!(
        stderr.contains("TRACE") || stderr.contains("DEBUG"),
        "{stderr}"
    );
}

#[test]
fn without_a_workspace_serve_exits_2_before_the_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(dir.path(), &["mcp", "serve"], &[]);
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no rustman workspace found"), "{stderr}");
}

#[test]
fn a_non_loopback_http_address_needs_allow_remote() {
    let ws = workspace();
    let child = spawn(ws.path(), &["mcp", "serve", "--http", "0.0.0.0:8765"], &[]);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--allow-remote is"), "{stderr}");
}

#[test]
fn no_flag_raises_agent_permissions() {
    let ws = workspace();
    for flag in ["--allow-mutations", "--allow-destructive", "--allow-host"] {
        let child = spawn(ws.path(), &["mcp", "serve", flag], &[]);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unexpected argument"), "{flag}: {stderr}");
    }
}

/// Kills the server when the test ends, even on failure.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The status line of a POST to `/mcp` on `port`.
fn post_status(port: u16, extra_headers: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = "{}";
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    response.trim_end().to_owned()
}

#[test]
fn a_loopback_http_server_starts_and_requires_its_token() {
    let ws = workspace();
    let token_file = ws.path().join("token.txt");
    std::fs::write(&token_file, "file-token-1234\n").unwrap();
    let token_arg = token_file.to_string_lossy().into_owned();
    let mut child = spawn(
        ws.path(),
        &[
            "mcp",
            "serve",
            "--http",
            "127.0.0.1:0",
            "--token-file",
            &token_arg,
        ],
        &[],
    );
    let stderr = child.stderr.take().unwrap();
    let server = Running(child);
    let mut stderr = BufReader::new(stderr);
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let url = line
        .trim()
        .strip_prefix("rustman mcp: listening on ")
        .unwrap_or_else(|| panic!("{line}"));
    assert!(
        url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp"),
        "{url}"
    );
    let port: u16 = url["http://127.0.0.1:".len()..url.len() - "/mcp".len()]
        .parse()
        .unwrap();
    assert_eq!(post_status(port, ""), "HTTP/1.1 401 Unauthorized");
    let wrong = "Authorization: Bearer not-it\r\n";
    assert_eq!(post_status(port, wrong), "HTTP/1.1 401 Unauthorized");
    let evil = "Authorization: Bearer file-token-1234\r\nOrigin: https://evil.example\r\n";
    assert_eq!(post_status(port, evil), "HTTP/1.1 403 Forbidden");
    // The right token gets past the guard to MCP, which rejects `{}`.
    let ok = post_status(port, "Authorization: Bearer file-token-1234\r\n");
    assert!(!ok.contains("401") && !ok.contains("403"), "{ok}");
    drop(server);
}

#[test]
fn without_a_token_file_a_generated_token_is_printed_once() {
    let ws = workspace();
    let mut child = spawn(ws.path(), &["mcp", "serve", "--http", "127.0.0.1:0"], &[]);
    let stderr = child.stderr.take().unwrap();
    let server = Running(child);
    let mut stderr = BufReader::new(stderr);
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    let token = first
        .trim()
        .strip_prefix("rustman mcp: bearer token (shown once): ")
        .unwrap_or_else(|| panic!("{first}"));
    assert_eq!(token.len(), 43, "{token}");
    let mut second = String::new();
    stderr.read_line(&mut second).unwrap();
    assert!(second.starts_with("rustman mcp: listening on "), "{second}");
    drop(server);
}
