//! `rustman send` (task 6.1): by path or name, `--body-only`, `-o`,
//! `--timeout`, `--insecure`, the ambiguous-name error, ad-hoc requests
//! outside a workspace, and the non-interactive OAuth2 login.

mod common;

use std::fs;

use common::{Server, closed_port, request, rustman, workspace, write};
use predicates::prelude::*;
use rustman_test_support::routes::{PNG, USER};
use rustman_test_support::{TestCerts, TestServer, Tls};

#[test]
fn send_by_name_prints_status_headers_and_body() {
    let server = Server::http();
    let ws = workspace(&server);
    let get_user = request("get-user", "{{base_url}}/users/42", "");
    write(ws.path(), "users/get-user.rman.yaml", &get_user);
    rustman(ws.path())
        .args(["send", "get-user", "--env", "dev"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("HTTP/1.1 200\n"))
        .stdout(predicate::str::contains("content-type: application/json\n"))
        .stdout(predicate::str::contains("x-request-id: req-1\n"))
        .stdout(predicate::str::ends_with(format!("\n\n{USER}")));
}

#[test]
fn send_by_path_and_by_id() {
    let server = Server::http();
    let ws = workspace(&server);
    let get_user = request("get-user", "{{base_url}}/users/42", "");
    write(ws.path(), "users/get-user.rman.yaml", &get_user);
    for target in ["users/get-user.rman.yaml", "users/get-user"] {
        rustman(ws.path())
            .args(["send", target, "--env", "dev", "--body-only"])
            .assert()
            .success()
            .stdout(USER);
    }
    rustman(&ws.path().join("users"))
        .args(["send", "get-user.rman.yaml", "--env", "dev", "--body-only"])
        .assert()
        .success()
        .stdout(USER);
}

#[test]
fn body_only_is_byte_for_byte() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "png.rman.yaml",
        &request("png", "{{base_url}}/png", ""),
    );
    let output = rustman(ws.path())
        .args(["send", "png", "--env", "dev", "--body-only"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, PNG);
}

#[test]
fn output_writes_the_body_to_a_file() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "png.rman.yaml",
        &request("png", "{{base_url}}/png", ""),
    );
    rustman(ws.path())
        .args(["send", "png", "--env", "dev", "-o", "out.png"])
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("wrote 20 bytes to out.png"));
    assert_eq!(fs::read(ws.path().join("out.png")).unwrap(), PNG);
}

#[test]
fn an_ambiguous_name_exits_2_with_both_paths() {
    let server = Server::http();
    let ws = workspace(&server);
    let list = request("List", "{{base_url}}/echo", "");
    write(ws.path(), "a/list.rman.yaml", &list);
    write(ws.path(), "b/list.rman.yaml", &list);
    rustman(ws.path())
        .args(["send", "List"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("error: request name 'List' is ambiguous: a/list.rman.yaml, b/list.rman.yaml\n");
}

#[test]
fn ad_hoc_requests_need_no_workspace() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["send", "POST", &server.url("/echo"), "--body-only"])
        .args(["-H", "X-Team: core", "--json", r#"{"a":1}"#])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let echo: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["body"], r#"{"a":1}"#);
    let headers = echo["headers"].as_array().unwrap();
    assert!(headers.iter().any(|h| h[0] == "x-team" && h[1] == "core"));
    // Nothing is written outside a workspace.
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    rustman(dir.path())
        .args(["send", &server.url("/users/42"), "--body-only"])
        .assert()
        .success()
        .stdout(USER);
}

#[test]
fn ad_hoc_requests_in_a_workspace_resolve_variables_and_secrets() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [tok]\n",
            server.url("")
        ),
    );
    rustman(ws.path())
        .args(["secret", "set", "tok", "--env", "dev"])
        .write_stdin("Qz9-c4n4ry")
        .assert()
        .success();
    rustman(ws.path())
        .args(["send", "GET", "{{base_url}}/echo?k={{tok}}", "--env", "dev"])
        .args(["--body-only", "--no-history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/echo?k=«redacted:tok»"));
    rustman(ws.path())
        .args(["send", "GET", "{{base_url}}/echo?k={{tok}}", "--env", "dev"])
        .args(["--body-only", "--no-history", "--show-secrets"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/echo?k=Qz9-c4n4ry"));
    rustman(ws.path())
        .args([
            "send",
            "{{base_url}}/users/42",
            "--env",
            "dev",
            "--no-history",
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("HTTP/1.1 200\n"));
}

#[test]
fn environments_need_a_workspace() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["send", "GET", &server.url("/echo"), "--env", "dev"])
        .assert()
        .code(2)
        .stderr("error: no rustman workspace found (run 'rustman init')\n");
    rustman(dir.path())
        .args(["send", "get-user"])
        .assert()
        .code(2)
        .stderr("error: no rustman workspace found (run 'rustman init')\n");
}

#[test]
fn ad_hoc_flags_need_an_ad_hoc_request() {
    let server = Server::http();
    let ws = workspace(&server);
    rustman(ws.path())
        .args(["send", "get-user", "-H", "A: b"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "-H, --data and --json need an ad-hoc request",
        ));
}

#[test]
fn timeout_overrides_the_request_setting() {
    let server = Server::http();
    let ws = workspace(&server);
    let slow = request("slow", "{{base_url}}/delay/3000", "");
    write(ws.path(), "slow.rman.yaml", &slow);
    rustman(ws.path())
        .args(["send", "slow", "--env", "dev", "--timeout", "200ms"])
        .assert()
        .code(3)
        .stdout("")
        .stderr("error: timeout after 200 ms (response)\n");
    rustman(ws.path())
        .args(["send", "slow", "--timeout", "soon"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid duration 'soon'"));
}

#[test]
fn insecure_skips_certificate_verification() {
    let certs = TestCerts::generate();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(TestServer::https(&certs, Tls::SelfSigned));
    let dir = tempfile::tempdir().unwrap();
    let url = server.url("/users/42");
    rustman(dir.path())
        .args(["send", &url])
        .assert()
        .code(3)
        .stderr(predicate::str::starts_with("error: tls:"));
    rustman(dir.path())
        .args(["send", &url, "--insecure", "--body-only"])
        .assert()
        .success()
        .stdout(USER);
    drop(server);
}

#[test]
fn a_failed_assertion_exits_1() {
    let server = Server::http();
    let ws = workspace(&server);
    let checked = request(
        "checked",
        "{{base_url}}/users/42",
        "assertions:\n  - status: 201\n",
    );
    write(ws.path(), "checked.rman.yaml", &checked);
    rustman(ws.path())
        .args(["send", "checked", "--env", "dev", "--body-only"])
        .assert()
        .code(1)
        .stdout(USER)
        .stderr("fail: status: expected 201, got 200\n");
}

#[test]
fn a_refused_connection_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:{}/", closed_port());
    rustman(dir.path())
        .args(["send", "GET", &url])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::starts_with("error: "));
}

#[test]
fn vars_override_the_environment() {
    let server = Server::http();
    let ws = workspace(&server);
    let echo = request("echo", "{{base_url}}/echo?who={{who}}", "");
    write(ws.path(), "echo.rman.yaml", &echo);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {}\n  who: env\n", server.url("")),
    );
    rustman(ws.path())
        .args([
            "send",
            "echo",
            "--env",
            "dev",
            "--var",
            "who=flag",
            "--body-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("/echo?who=flag"));
    rustman(ws.path())
        .args(["send", "echo", "--env", "dev", "--var", "oops"])
        .assert()
        .code(2);
}

#[test]
fn oauth2_login_without_a_terminal_fails_with_the_hint() {
    let server = Server::http();
    let ws = workspace(&server);
    let profile = request(
        "Profile",
        "{{base_url}}/me",
        "auth:\n  type: oauth2\n  grant: authorization_code\n  \
         auth_url: http://127.0.0.1:1/authorize\n  token_url: http://127.0.0.1:1/token\n  \
         client_id: rustman-cli\n",
    );
    write(ws.path(), "http/oauth-login.rman.yaml", &profile);
    // assert_cmd gives the child no terminal on stdin.
    rustman(ws.path())
        .args(["send", "Profile", "--env", "dev"])
        .assert()
        .code(3)
        .stderr(
            "error: oauth2: interactive login required (run 'rustman auth login http/oauth-login')\n",
        );
}
