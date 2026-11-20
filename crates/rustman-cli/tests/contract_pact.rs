//! `rustman contract pact generate` and `rustman contract pact verify`
//! (add-contract-testing tasks 5.2, 6.2, 7.3) against the in-process test
//! server: `/users/42` answers the user 42 and `/echo` (the state-change
//! endpoint here) answers any method with 200.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, closed_port, request, rustman, valid_report, workspace, write};
use serde_json::{Value, json};

const TOKEN: &str = "canary-pact-5c1e";

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A workspace with `users/get-user.rman.yaml` (bearer `{{token}}`, a
/// secret) and a graphql request, and the secret stored.
fn pact_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
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
        "users/get-user.rman.yaml",
        &request(
            "get user",
            "{{base_url}}/users/42",
            "headers:\n  - name: Authorization\n    value: \"Bearer {{token}}\"\n\
             assertions:\n  - status: 200\n  - json: {path: \"$.id\", equals: 42}\n  - latency: {total_lt: 300}\n",
        ),
    );
    write(
        ws.path(),
        "users/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: \"{{base_url}}/graphql\"\ndocument: \"{ me { id } }\"\n",
    );
    let stored = rustman(ws.path())
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(format!("{TOKEN}\n"))
        .output()
        .unwrap();
    assert!(stored.status.success(), "{}", stderr(&stored));
    ws
}

fn generate(ws: &Path, extra: &[&str]) -> std::process::Output {
    rustman(ws)
        .args([
            "contract",
            "pact",
            "generate",
            "users",
            "--consumer",
            "web",
            "--provider",
            "users",
            "--env",
            "dev",
        ])
        .args(extra)
        .output()
        .unwrap()
}

fn pact_file(ws: &Path) -> Value {
    let text = fs::read_to_string(ws.join("contracts/pacts/web-users.json")).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn generate_writes_the_default_file_without_secrets() {
    let server = Server::http();
    let ws = pact_workspace(&server);
    let output = generate(ws.path(), &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "wrote contracts/pacts/web-users.json (1 interactions, Pact 4.0)\n"
    );
    let warnings = stderr(&output);
    assert!(
        warnings.contains(
            "warning: latency assertion has no Pact equivalent (users/get-user.rman.yaml)\n"
        ),
        "{warnings}"
    );
    assert!(warnings.contains("kind graphql skipped"), "{warnings}");
    assert!(!warnings.contains(TOKEN));
    let pact = pact_file(ws.path());
    let interaction = &pact["interactions"][0];
    assert_eq!(interaction["request"]["path"], "/users/42");
    assert_eq!(
        interaction["request"]["headers"]["Authorization"],
        "Bearer «redacted:token»"
    );
    assert_eq!(
        interaction["request"]["matchingRules"]["header"]["Authorization"]["matchers"][0],
        json!({"match": "regex", "regex": "^Bearer .+$"})
    );
}

#[test]
fn generate_writes_v3_on_request() {
    let server = Server::http();
    let ws = pact_workspace(&server);
    let output = generate(
        ws.path(),
        &["--pact-version", "3", "--output", "out/p.json"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let pact: Value =
        serde_json::from_str(&fs::read_to_string(ws.path().join("out/p.json")).unwrap()).unwrap();
    assert_eq!(pact["metadata"]["pactSpecification"]["version"], "3.0.0");
}

#[test]
fn generate_refuses_names_that_are_not_file_names() {
    let server = Server::http();
    let ws = pact_workspace(&server);
    let output = rustman(ws.path())
        .args([
            "contract",
            "pact",
            "generate",
            "--consumer",
            "../web",
            "--provider",
            "users",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid pacticipant name '../web'"));
}

#[test]
fn a_generated_pact_verifies_against_the_provider() {
    let server = Server::http();
    let ws = pact_workspace(&server);
    assert!(generate(ws.path(), &[]).status.success());
    let output = rustman(ws.path())
        .args([
            "contract",
            "pact",
            "verify",
            "contracts/pacts/web-users.json",
        ])
        .args([
            "--provider-url",
            &server.url(""),
            "--report",
            "json=out/pact.json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "passed   get user (contracts/pacts/web-users.json)\n\n1 passed, 0 failed, 0 blocked\n"
    );
    let report = valid_report(&fs::read(ws.path().join("out/pact.json")).unwrap());
    assert_eq!(report["kind"], "contract");
    assert_eq!(report["mode"], "pact-verify");
    assert_eq!(report["items"][0]["status"], "passed");
}

/// A hand-written Pact file in `dir`.
fn pact(dir: &Path, interactions: Value) -> String {
    let text = json!({
        "consumer": {"name": "web"},
        "provider": {"name": "users"},
        "interactions": interactions,
        "metadata": {"pactSpecification": {"version": "3.0.0"}},
    });
    write(dir, "p.json", &text.to_string());
    "p.json".to_owned()
}

#[test]
fn a_mismatch_fails_with_its_json_path() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    let file = pact(
        dir.path(),
        json!([{"description": "user as text", "request": {"method": "GET", "path": "/users/42"},
                "response": {"status": 200, "body": {"id": "42"}}}]),
    );
    let output = rustman(dir.path())
        .args([
            "contract",
            "pact",
            "verify",
            &file,
            "--provider-url",
            &server.url(""),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).contains("  $.id: expected string \"42\", got number 42\n"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn provider_states_need_a_state_url() {
    let dir = tempfile::tempdir().unwrap();
    let file = pact(
        dir.path(),
        json!([{"description": "d", "providerState": "user 42 exists",
                "request": {"method": "GET", "path": "/users/42"}, "response": {"status": 200}}]),
    );
    let url = format!("http://127.0.0.1:{}", closed_port());
    let output = rustman(dir.path())
        .args(["contract", "pact", "verify", &file, "--provider-url", &url])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "error: provider states declared but --provider-states-url not set\n"
    );
}

#[test]
fn states_are_posted_to_the_state_url() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    let file = pact(
        dir.path(),
        json!([{"description": "d", "providerState": "user 42 exists",
                "request": {"method": "GET", "path": "/users/42"}, "response": {"status": 200}}]),
    );
    let output = rustman(dir.path())
        .args([
            "contract",
            "pact",
            "verify",
            &file,
            "--provider-url",
            &server.url(""),
        ])
        .args(["--provider-states-url", &server.url("/echo")])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
}

#[test]
fn a_mutating_interaction_is_blocked_with_exit_4() {
    let dir = tempfile::tempdir().unwrap();
    let file = pact(
        dir.path(),
        json!([{"description": "create user", "request": {"method": "POST", "path": "/users"},
                "response": {"status": 201}}]),
    );
    let url = format!("http://127.0.0.1:{}", closed_port());
    let output = rustman(dir.path())
        .args(["contract", "pact", "verify", &file, "--provider-url", &url])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        stdout(&output),
        "blocked  create user (p.json): blocked: mutations not allowed\n\n0 passed, 0 failed, 1 blocked\n"
    );
}

/// A production provider is refused unless `--production` repeats its
/// host, as the refusal says.
#[test]
fn a_production_provider_needs_the_production_flag() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[safety]\nproduction_hosts = [\"localhost\"]\n",
    );
    let file = pact(
        ws.path(),
        json!([{"description": "d", "request": {"method": "GET", "path": "/users/42"},
                "response": {"status": 200}}]),
    );
    let url = server.url("");
    let verify = |extra: &[&str]| {
        rustman(ws.path())
            .args(["contract", "pact", "verify", &file, "--provider-url", &url])
            .args(extra)
            .output()
            .unwrap()
    };
    let refused = verify(&[]);
    assert_eq!(refused.status.code(), Some(4), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("--production localhost"),
        "{}",
        stderr(&refused)
    );
    let confirmed = verify(&["--production", "localhost"]);
    assert_eq!(
        confirmed.status.code(),
        Some(0),
        "{}{}",
        stdout(&confirmed),
        stderr(&confirmed)
    );
}

/// A provider that echoes a secret back: the mismatch quoting it is
/// redacted on stdout and in every report.
#[test]
fn an_echoed_secret_never_reaches_the_output() {
    let server = Server::http();
    let ws = pact_workspace(&server);
    let file = pact(
        ws.path(),
        json!([{"description": "echo", "request": {"method": "GET", "path": "/echo",
                    "headers": {"Content-Type": "text/plain"}, "body": TOKEN},
                "response": {"status": 200, "body": {"body": "something else"}}}]),
    );
    let output = rustman(ws.path())
        .args([
            "contract",
            "pact",
            "verify",
            &file,
            "--provider-url",
            &server.url(""),
            "--env",
            "dev",
        ])
        .args([
            "--report",
            "json=out/p.json",
            "--report",
            "junit=out/p.xml",
            "--report",
            "markdown=out/p.md",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains(
            "$.body: expected string \"something else\", got string \"«redacted:token»\""
        ),
        "{text}"
    );
    for written in [text, stderr(&output)].into_iter().chain(
        ["out/p.json", "out/p.xml", "out/p.md"]
            .map(|f| fs::read_to_string(ws.path().join(f)).unwrap()),
    ) {
        assert!(!written.contains(TOKEN), "{written}");
    }
}
