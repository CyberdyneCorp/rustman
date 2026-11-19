//! `rustman contract verify` (add-contract-testing tasks 7.1, 7.3): history
//! recorded by `rustman send` against the test server, or a run report,
//! checked against a document. The check itself sends nothing.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, request, rustman, valid_report, workspace, write};

const TOKEN: &str = "canary-conf-8d2a";

/// `/users/{id}` returns a `User` that needs a `phone` the test server's
/// user lacks; `/health` is documented and never called.
const SPEC: &str = r#"openapi: 3.1.0
info: {title: Users, version: "1"}
paths:
  /users/{id}:
    get:
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
      responses:
        "200":
          description: A user
          content:
            application/json:
              schema:
                type: object
                required: [id, email, phone]
                properties:
                  id: {type: integer}
                  email: {type: string}
                  phone: {type: string}
  /health:
    get:
      responses:
        "200": {description: Up}
"#;

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A workspace with the spec, a `get-user` and an `echo` request (which
/// sends the secret `token` and gets it echoed back), and both sent once.
fn recorded(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    write(ws.path(), "openapi.yaml", SPEC);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.url("")
        ),
    );
    write(
        ws.path(),
        "get-user.rman.yaml",
        &request("get user", "{{base_url}}/users/42", ""),
    );
    write(
        ws.path(),
        "echo.rman.yaml",
        &request(
            "echo",
            "{{base_url}}/echo",
            "method: POST\nbody:\n  type: json\n  content: {token: \"{{token}}\"}\n",
        ),
    );
    let stored = rustman(ws.path())
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(format!("{TOKEN}\n"))
        .output()
        .unwrap();
    assert!(stored.status.success(), "{}", stderr(&stored));
    for name in ["get user", "echo"] {
        let sent = rustman(ws.path())
            .args(["send", name, "--env", "dev"])
            .output()
            .unwrap();
        assert!(sent.status.success(), "{}", stderr(&sent));
    }
    ws
}

fn verify(ws: &Path, extra: &[&str]) -> std::process::Output {
    rustman(ws)
        .args([
            "contract",
            "verify",
            "--spec",
            "openapi.yaml",
            "--env",
            "dev",
        ])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn history_is_checked_with_violations_coverage_and_undocumented_traffic() {
    let server = Server::http();
    let ws = recorded(&server);
    let output = verify(ws.path(), &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.starts_with("violation     GET /users/{id}: exchange "),
        "{text}"
    );
    assert!(
        text.contains("\n  $.phone: required property missing\n"),
        "{text}"
    );
    assert!(
        text.contains("undocumented  POST /echo: exchange "),
        "{text}"
    );
    assert!(
        text.contains(
            "\nobserved operations:\n  GET /users/{id}  1 exchanges, 1 with violations\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n1/2 operations observed; 1 violation, 1 undocumented, 0 conforms\n"),
        "{text}"
    );
}

#[test]
fn undocumented_traffic_fails_only_with_the_flag() {
    let server = Server::http();
    let ws = recorded(&server);
    let only_echo = ["--grep", "/echo"];
    assert_eq!(verify(ws.path(), &only_echo).status.code(), Some(0));
    let strict = [&only_echo[..], &["--fail-on-undocumented"]].concat();
    assert_eq!(verify(ws.path(), &strict).status.code(), Some(1));
}

#[test]
fn the_report_holds_the_evidence_redacted() {
    let server = Server::http();
    let ws = recorded(&server);
    let output = verify(
        ws.path(),
        &[
            "--report",
            "json=out/c.json",
            "--report",
            "markdown=out/c.md",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    for file in ["out/c.json", "out/c.md"] {
        let text = fs::read_to_string(ws.path().join(file)).unwrap();
        assert!(!text.contains(TOKEN), "{file}: {text}");
    }
    assert!(!stdout(&output).contains(TOKEN));
    let report = valid_report(&fs::read(ws.path().join("out/c.json")).unwrap());
    assert_eq!(report["mode"], "conformance");
    assert_eq!(report["coverage"]["observed"], 1);
    assert_eq!(report["coverage"]["total"], 2);
    let markdown = fs::read_to_string(ws.path().join("out/c.md")).unwrap();
    assert!(markdown.contains("1/2 operations observed"), "{markdown}");
}

#[test]
fn a_run_report_is_a_source_too() {
    let server = Server::http();
    let ws = workspace(&server);
    write(ws.path(), "openapi.yaml", SPEC);
    write(
        ws.path(),
        "get-user.rman.yaml",
        &request("get user", "{{base_url}}/users/42", ""),
    );
    let run = rustman(ws.path())
        .args([
            "run",
            "--env",
            "dev",
            "--no-history",
            "--report",
            "json=out/run.json",
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    let output = rustman(ws.path())
        .args([
            "contract",
            "verify",
            "--spec",
            "openapi.yaml",
            "--from",
            "out/run.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("exchange get-user#1"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_report_of_another_kind_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "openapi.yaml", SPEC);
    write(
        dir.path(),
        "c.json",
        r#"{"schema": "rustman.report/v1", "kind": "contract", "rustman_version": "0", "mode": "diff", "sources": [],
            "summary": {"counts": [], "allowed": 0, "failing": 0, "passed": true}, "items": []}"#,
    );
    let output = rustman(dir.path())
        .args([
            "contract",
            "verify",
            "--spec",
            "openapi.yaml",
            "--from",
            "c.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "error: c.json: a run report is needed, not a contract report\n"
    );
}

/// A local port nothing should connect to, and whether anything did.
struct Listener(std::net::TcpListener);

impl Listener {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Self(listener)
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.0.local_addr().unwrap())
    }

    fn connected(&self) -> bool {
        match self.0.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(e) => panic!("{e}"),
        }
    }
}

/// Scenario "No network traffic": a URL `--spec` and a remote `$ref` are
/// refused before any connection is opened.
#[test]
fn the_document_is_never_fetched_over_the_network() {
    let listener = Listener::new();
    let dir = tempfile::tempdir().unwrap();
    let url = listener.url("/openapi.yaml");
    let output = rustman(dir.path())
        .args(["contract", "verify", "--spec", &url])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("opens no network connection"),
        "{}",
        stderr(&output)
    );
    let remote = format!(
        "openapi: 3.1.0\ninfo: {{title: T, version: \"1\"}}\npaths:\n  /a:\n    get:\n      responses:\n        \"200\": {{$ref: \"{}\"}}\n",
        listener.url("/responses.yaml#/ok")
    );
    write(dir.path(), "remote.yaml", &remote);
    let output = rustman(dir.path())
        .args(["contract", "verify", "--spec", "remote.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("is not resolved"),
        "{}",
        stderr(&output)
    );
    assert!(!listener.connected());
}

/// Scenario "No network traffic": a full check, with the document's server
/// pointing at a live port, connects to nothing.
#[test]
fn checking_history_opens_no_connection() {
    let server = Server::http();
    let ws = recorded(&server);
    let listener = Listener::new();
    let with_server = SPEC.replace(
        "paths:\n",
        &format!("servers: [{{url: \"{}\"}}]\npaths:\n", listener.url("")),
    );
    write(ws.path(), "openapi.yaml", &with_server);
    let output = verify(ws.path(), &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(!listener.connected());
}
