//! `rustman record` and `rustman record ca` (traffic-recorder spec: Forward
//! proxy command, Loopback by default, Local CA generation and storage, CA
//! is never trusted automatically). The tests use the file secret store, so
//! no OS keychain or trust store is ever touched.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{Server, rustman, write};
use rustman_test_support::certs::TestCerts;

const WORKSPACE_ID: &str = "0b5c0b9e-58c4-4c1e-9f0e-6f1f4b7f2a10";

/// A workspace with a known id, and a config directory holding the public
/// certificate of its CA (the key would be in the keychain).
fn workspace_with_ca_certificate(pem: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    write(
        ws.path(),
        ".rustman/workspace-id",
        &format!("{WORKSPACE_ID}\n"),
    );
    let config = tempfile::tempdir().unwrap();
    write(
        config.path(),
        &format!("rustman/record-ca/{WORKSPACE_ID}.pem"),
        pem,
    );
    (ws, config)
}

/// `rustman` in `dir` with its user config directory at `config`.
fn rustman_with_config(dir: &Path, config: &Path) -> assert_cmd::Command {
    let mut cmd = rustman(dir);
    cmd.env("XDG_CONFIG_HOME", config).env("APPDATA", config);
    cmd
}

/// Every file under `dir`, relative and sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_network_address_needs_allow_remote() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["record", "--host", "0.0.0.0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: record: binding a non-loopback address requires --allow-remote"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn a_busy_port_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = rustman(dir.path())
        .args(["record", "--port", &port])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        format!("error: record: cannot bind 127.0.0.1:{port}: address in use")
    );
}

/// Lines of `pipe`, read on a thread.
fn lines(pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
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

fn spawn_record(dir: &Path, args: &[&str]) -> Child {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .args(["record"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn next(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(30)).expect("a line")
}

#[test]
fn record_listens_on_loopback_and_prints_the_exports_and_redacted_captures() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_record(dir.path(), &["--port", "0"]);
    let out = lines(child.stdout.take().unwrap());
    let banner = next(&out);
    let addr = banner
        .strip_prefix("Recording proxy on http://")
        .unwrap_or_else(|| panic!("{banner}"))
        .to_owned();
    assert!(
        addr.starts_with("127.0.0.1:"),
        "loopback by default: {addr}"
    );
    assert_eq!(next(&out), format!("export HTTP_PROXY=http://{addr}"));
    assert_eq!(next(&out), format!("export HTTPS_PROXY=http://{addr}"));

    let token = "s3cr3t-t0ken-value";
    let url = server.url(&format!("/echo?echo={token}"));
    let mut tcp = TcpStream::connect(&addr).unwrap();
    let head = format!(
        "GET {url} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    );
    tcp.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let line = next(&out);
    child.kill().unwrap();
    let _ = child.wait();
    let shown_url = server.url("/echo?echo=«redacted:Authorization»");
    assert!(line.starts_with(&format!("GET {shown_url} 200 ")), "{line}");
    assert!(!line.contains(token), "{line}");
}

#[test]
fn ca_init_without_a_keychain_exits_2_and_writes_nothing() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    let config = tempfile::tempdir().unwrap();
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "ca", "init"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: record: a keychain is required to store the CA private key"
    );
    assert_eq!(files(ws.path()), ["rustman.toml"]);
    assert!(files(config.path()).is_empty());
}

#[test]
fn intercepting_without_a_ca_exits_2() {
    let (ws, config) = workspace_with_ca_certificate("");
    std::fs::remove_dir_all(config.path().join("rustman")).unwrap();
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "--port", "0", "--intercept", "localhost"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error: record: --intercept needs a local CA"),
        "{stderr}"
    );
}

#[test]
fn interception_scope_from_rustman_toml_is_warned_about() {
    let (ws, config) = workspace_with_ca_certificate("");
    std::fs::remove_dir_all(config.path().join("rustman")).unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n[record]\nintercept = [\"*\"]\n",
    );
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "--port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with(
            "warning: [record] intercept in rustman.toml decrypts HTTPS to *; \
             pass --intercept to choose the hosts yourself\n"
        ),
        "{stderr}"
    );
}

#[test]
fn ca_export_writes_exactly_one_certificate_and_no_key() {
    let certs = TestCerts::generate();
    let (ws, config) = workspace_with_ca_certificate(&certs.ca_pem);
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "ca", "export", "--out", "ca.pem"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let pem = std::fs::read_to_string(ws.path().join("ca.pem")).unwrap();
    assert_eq!(pem.matches("-----BEGIN CERTIFICATE-----").count(), 1);
    assert_eq!(pem.matches("-----BEGIN ").count(), 1, "no key: {pem}");
    assert!(!pem.contains("PRIVATE KEY"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let fingerprint = stdout
        .lines()
        .find_map(|l| l.strip_prefix("SHA-256: "))
        .unwrap();
    assert_eq!(fingerprint.split(':').count(), 32, "{stdout}");

    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "ca", "export"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), pem);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.trim_end(), format!("SHA-256: {fingerprint}"));
}

#[test]
fn ca_export_without_a_ca_exits_2() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    let config = tempfile::tempdir().unwrap();
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "ca", "export"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: record: this workspace has no local CA; run 'rustman record ca init'"
    );
}

/// `rustman` in `ws` with the stand-in keychain of test builds in
/// `keychain` (never the OS keychain) and the user config in `config`.
fn rustman_with_test_keychain(ws: &Path, config: &Path, keychain: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"));
    cmd.current_dir(ws)
        .env_remove("RUSTMAN_SECRET_STORE")
        .env("RUSTMAN_RECORD_TEST_KEYCHAIN", keychain)
        .env("XDG_CONFIG_HOME", config)
        .env("APPDATA", config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// The certificate path of the CA that `ca init` created for `ws`.
fn ca_certificate(ws: &Path, config: &Path) -> std::path::PathBuf {
    let id = std::fs::read_to_string(ws.join(".rustman/workspace-id")).unwrap();
    config.join(format!("rustman/record-ca/{}.pem", id.trim()))
}

#[test]
fn ca_remove_deletes_the_key_and_the_certificate_after_confirmation() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    let config = tempfile::tempdir().unwrap();
    let keychain = tempfile::tempdir().unwrap();
    let run = |args: &[&str], stdin: &str| {
        let mut cmd = rustman_with_test_keychain(ws.path(), config.path(), keychain.path());
        let mut child = cmd.args(args).stdin(Stdio::piped()).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let init = run(&["record", "ca", "init"], "");
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let cert = ca_certificate(ws.path(), config.path());
    assert!(cert.is_file());
    assert_eq!(
        files(keychain.path()).len(),
        1,
        "the key is in the keychain"
    );

    let declined = run(&["record", "ca", "remove"], "no\n");
    assert_eq!(declined.status.code(), Some(1), "{declined:?}");
    let stdout = String::from_utf8_lossy(&declined.stdout);
    assert!(
        stdout.contains("Not removed; nothing was changed."),
        "{stdout}"
    );
    assert!(cert.is_file());
    assert_eq!(files(keychain.path()).len(), 1);

    let removed = run(&["record", "ca", "remove", "--yes"], "");
    assert_eq!(removed.status.code(), Some(0), "{removed:?}");
    let stdout = String::from_utf8_lossy(&removed.stdout);
    assert!(stdout.contains("Deleted the CA"), "{stdout}");
    assert!(!cert.exists());
    assert!(files(keychain.path()).is_empty(), "the key is deleted");

    let again = run(&["record", "ca", "remove", "--yes"], "");
    assert_eq!(again.status.code(), Some(2), "{again:?}");
}

/// Task 8.1, on every CI platform: curl through `rustman record
/// --intercept localhost --save --har out.har` to a local TLS API. The CA
/// key goes to the stand-in keychain of test builds (never the OS
/// keychain nor a trust store: curl gets the CA with `--cacert`), and the
/// recording stops by itself after one capture, as a test cannot send
/// Ctrl+C to a child on Windows.
#[test]
fn curl_through_an_intercepting_recorder_saves_request_files_without_secrets() {
    let Some(mut curl) = curl() else {
        assert!(std::env::var_os("CI").is_none(), "CI needs curl");
        eprintln!("skipped: curl is not installed");
        return;
    };
    let api = tls_api();
    let ws = workspace_trusting(&api);
    let config = tempfile::tempdir().unwrap();
    let keychain = tempfile::tempdir().unwrap();
    let rustman = || rustman_with_test_keychain(ws.path(), config.path(), keychain.path());
    let init = rustman().args(["record", "ca", "init"]).output().unwrap();
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let cert = ca_certificate(ws.path(), config.path());

    let mut child = rustman()
        .env("RUSTMAN_RECORD_TEST_STOP_AFTER", "1")
        .args(["record", "--port", "0", "--intercept", "localhost"])
        .args(["--save", "--har", "out.har"])
        .spawn()
        .unwrap();
    let out = lines(child.stdout.take().unwrap());
    let err = lines(child.stderr.take().unwrap());
    let addr = banner_addr(&next(&out));
    let body = config.path().join("body.out");
    if cfg!(windows) {
        // Schannel cannot check the revocation of a local CA's leaves.
        curl.arg("--ssl-no-revoke");
    }
    let output = curl
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .args(["-sS", "-w", "%{http_code}", "-o"])
        .arg(&body)
        .args(["-x", &format!("http://{addr}")])
        .arg("--cacert")
        .arg(&cert)
        .args(["-H", &format!("Authorization: Bearer {BEARER}")])
        .args(["-H", &format!("X-Api-Key: {X_API_KEY}")])
        .args(["-H", "Content-Type: application/json"])
        .args(["-d", &format!(r#"{{"password":"{PASSWORD}"}}"#)])
        .arg(format!(
            "https://localhost:{}/echo?api_key={API_KEY}",
            api.server.port()
        ))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "200", "{output:?}");
    assert_eq!(child.wait().unwrap().code(), Some(0));
    let stdout: Vec<String> = out.iter().collect();
    let stderr: Vec<String> = err.iter().collect();
    let port = api.server.port();
    let file = std::fs::read_to_string(
        ws.path()
            .join(format!("recorded/localhost-{port}/post-echo.rman.yaml")),
    )
    .unwrap();
    for placeholder in ["Bearer {{auth_token}}", "{{api_key}}", "{{password}}"] {
        assert!(file.contains(placeholder), "{placeholder}: {file}");
    }
    let env = std::fs::read_to_string(ws.path().join("environments/recorded.yaml")).unwrap();
    assert!(
        env.contains(&format!("base_url: https://localhost:{port}")),
        "{env}"
    );
    let har: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ws.path().join("out.har")).unwrap()).unwrap();
    let entry = &har["log"]["entries"][0];
    assert_eq!(
        entry["comment"], "decrypted by the Rustman local CA",
        "{har}"
    );
    assert_no_raw_secret(ws.path(), stdout.iter().chain(&stderr));
}

#[test]
fn ca_uninstall_with_nothing_installed_changes_nothing() {
    let certs = TestCerts::generate();
    let (ws, config) = workspace_with_ca_certificate(&certs.ca_pem);
    let output = rustman_with_config(ws.path(), config.path())
        .args(["record", "ca", "uninstall"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        "Rustman did not install this workspace's CA in any trust store."
    );
}

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// GitHub's anchor for a heading: lower case, punctuation dropped,
/// spaces as hyphens.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[test]
fn record_docs_links_resolve_and_errors_match() {
    let record = doc("record.md");
    let anchors: Vec<String> = record
        .lines()
        .filter_map(|l| l.strip_prefix('#'))
        .map(|l| slug(l.trim_start_matches('#')))
        .collect();
    let cli = doc("cli.md");
    let links = cli
        .split("record.md#")
        .skip(1)
        .chain(record.split("](#").skip(1));
    for link in links {
        let anchor: String = link.chars().take_while(|c| *c != ')').collect();
        assert!(anchors.contains(&anchor), "no heading for #{anchor}");
    }
    assert!(doc("README.md").contains("(record.md)"));
    for line in [
        "error: record: cannot bind 127.0.0.1:8888: address in use",
        "error: record: binding a non-loopback address requires --allow-remote",
        "export HTTPS_PROXY=http://127.0.0.1:8888",
        "record: a keychain is required to store the CA private key",
    ] {
        assert!(record.contains(line), "{line}");
    }
}

/// Send `head` (a full request) to the proxy at `addr` and read the reply.
fn proxy_request(addr: &str, head: &str) -> String {
    let mut tcp = TcpStream::connect(addr).unwrap();
    tcp.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).unwrap();
    response
}

fn banner_addr(line: &str) -> String {
    line.strip_prefix("Recording proxy on http://")
        .unwrap_or_else(|| panic!("{line}"))
        .to_owned()
}

#[test]
fn json_lines_are_redacted_and_alone_on_stdout() {
    let server = Server::http();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_record(dir.path(), &["--port", "0", "--json"]);
    let out = lines(child.stdout.take().unwrap());
    let err = lines(child.stderr.take().unwrap());
    let addr = banner_addr(&next(&err));

    let token = "json-t0ken-value-123";
    let url = server.url("/echo");
    let head = format!(
        "GET {url} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
         Cookie: sid=c00kie-value-456\r\nConnection: close\r\n\r\n"
    );
    assert!(proxy_request(&addr, &head).starts_with("HTTP/1.1 200"));

    let line = next(&out);
    child.kill().unwrap();
    let _ = child.wait();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["method"], "GET");
    assert_eq!(value["url"], url);
    assert_eq!(value["status"], 200);
    assert!(value["duration_ms"].is_u64(), "{line}");
    let headers = value["request_headers"].as_array().unwrap();
    let shown = |name: &str| {
        headers
            .iter()
            .find(|h| h["name"] == name)
            .map(|h| h["value"].as_str().unwrap().to_owned())
    };
    assert_eq!(
        shown("authorization").as_deref(),
        Some("«redacted:Authorization»")
    );
    assert_eq!(shown("cookie").as_deref(), Some("«redacted:Cookie»"));
    for secret in [token, "c00kie-value-456"] {
        assert!(!line.contains(secret), "{line}");
    }
}

#[test]
fn save_flags_need_save_and_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["record", "--port", "0", "--save"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "error: record: --save needs a workspace; run 'rustman init' or pass --workspace"
    );
    for args in [
        &["--into", "x"][..],
        &["--store-secrets"],
        &["--insecure"],
        &["--show-secrets"],
    ] {
        let output = rustman(dir.path())
            .args(["record", "--port", "0"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

/// A `--save` session that recorded nothing writes nothing and says so,
/// instead of `Saved 0 request files`.
#[cfg(unix)]
#[test]
fn ctrl_c_on_an_empty_session_saves_nothing() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    let mut child = spawn_record(ws.path(), &["--port", "0", "--save"]);
    let out = lines(child.stdout.take().unwrap());
    let err = lines(child.stderr.take().unwrap());
    next(&out);
    assert_eq!(interrupt(&mut child), Some(0));
    let stdout: Vec<String> = out.iter().collect();
    let stderr: Vec<String> = err.iter().collect();
    assert!(
        stderr
            .iter()
            .any(|l| l == "No requests recorded; nothing saved"),
        "{stderr:?}"
    );
    assert!(
        stdout
            .iter()
            .chain(&stderr)
            .all(|l| !l.starts_with("Saved ")),
        "{stdout:?} {stderr:?}"
    );
    assert!(!ws.path().join("environments").exists());
    assert!(!ws.path().join("recorded").exists());
}

#[cfg(unix)]
#[test]
fn ctrl_c_on_a_recorded_session_saves_request_files_without_secrets() {
    let server = Server::http();
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    let mut child = spawn_record(
        ws.path(),
        &[
            "--port",
            "0",
            "--save",
            "--env",
            "staging",
            "--har",
            "out/session.har",
        ],
    );
    let out = lines(child.stdout.take().unwrap());
    let addr = banner_addr(&next(&out));
    next(&out);
    next(&out);

    let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2ln";
    for id in [17, 42] {
        let url = server.url(&format!("/users/{id}"));
        let head = format!(
            "GET {url} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
             Connection: close\r\n\r\n"
        );
        proxy_request(&addr, &head);
        next(&out);
    }
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let wrote = next(&out);
    let summary = next(&out);
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_eq!(
        summary,
        "Saved 1 request file (0 unchanged), environment 'staging'"
    );
    assert!(wrote.starts_with("Wrote 2 captures to "), "{wrote}");
    let har: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ws.path().join("out/session.har")).unwrap())
            .unwrap();
    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(har["log"]["version"], "1.2");
    assert_eq!(entries.len(), 2);
    let auth = entries[0]["request"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "authorization")
        .unwrap();
    assert_eq!(auth["value"], "«redacted:Authorization»");

    let port = server.server.port();
    let file = std::fs::read_to_string(
        ws.path()
            .join(format!("recorded/localhost-{port}/get-users-id.rman.yaml")),
    )
    .unwrap();
    assert!(file.contains("{{base_url}}/users/{{user_id}}"), "{file}");
    assert!(file.contains("Bearer {{auth_token}}"), "{file}");
    let env = std::fs::read_to_string(ws.path().join("environments/staging.yaml")).unwrap();
    assert!(
        env.contains(&format!("base_url: http://localhost:{port}")),
        "{env}"
    );
    for rel in files(ws.path()) {
        let text = std::fs::read_to_string(ws.path().join(&rel)).unwrap_or_default();
        assert!(!text.contains("eyJ"), "raw token in {rel}");
    }
}

/// An HTTPS test API signed by a private CA, on its own runtime.
struct TlsApi {
    server: rustman_test_support::TestServer,
    certs: TestCerts,
    _runtime: tokio::runtime::Runtime,
}

fn tls_api() -> TlsApi {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let certs = TestCerts::generate();
    let server = runtime.block_on(rustman_test_support::TestServer::https(
        &certs,
        rustman_test_support::Tls::Http1Only,
    ));
    TlsApi {
        server,
        certs,
        _runtime: runtime,
    }
}

/// A workspace whose `[http] ca_bundle` trusts `api`'s private CA.
fn workspace_trusting(api: &TlsApi) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n\n[http]\nca_bundle = \"certs/api-ca.pem\"\n",
    );
    write(ws.path(), "certs/api-ca.pem", &api.certs.ca_pem);
    ws
}

#[test]
fn the_upstream_is_verified_with_the_workspace_ca_bundle() {
    let api = tls_api();
    let upstream = format!("https://localhost:{}", api.server.port());
    let request = "GET /users/42 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let trusting = workspace_trusting(&api);
    let plain = tempfile::tempdir().unwrap();
    write(
        plain.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    );
    for (ws, status) in [(trusting.path(), "200"), (plain.path(), "502")] {
        let mut child = spawn_record(ws, &["--port", "0", "--upstream", &upstream]);
        let out = lines(child.stdout.take().unwrap());
        let banner = next(&out);
        let addr = banner
            .strip_prefix("Recording reverse proxy on http://")
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or_else(|| panic!("{banner}"))
            .to_owned();
        let response = proxy_request(&addr, request);
        child.kill().unwrap();
        let _ = child.wait();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{status}: {response}"
        );
    }
}

#[test]
fn an_unreadable_ca_bundle_exits_2() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"rec\"\nversion = 1\n\n[http]\nca_bundle = \"missing.pem\"\n",
    );
    let output = rustman(ws.path())
        .args(["record", "--port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error: record: cannot read [http] ca_bundle"),
        "{stderr}"
    );
}

/// `curl`, when this machine has it (Windows ships `curl.exe`).
fn curl() -> Option<std::process::Command> {
    let found = std::process::Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    found.then(|| std::process::Command::new("curl"))
}

/// Stop a recording with Ctrl+C (SIGINT); its exit code.
#[cfg(unix)]
fn interrupt(child: &mut Child) -> Option<i32> {
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    child.wait().unwrap().code()
}

/// Every file under `dir` with its contents.
fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    files(dir)
        .into_iter()
        .map(|rel| {
            let bytes = std::fs::read(dir.join(&rel)).unwrap();
            (rel, bytes)
        })
        .collect()
}

const API_KEY: &str = "QKEYSECRET55abcdef";
const PASSWORD: &str = "hunter2hunter2";
const X_API_KEY: &str = "sk_live_ABCDEF1234567890abcdef";
const BEARER: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2ln";

/// One `rustman record --upstream ... --save --json --har` session in
/// `ws` with one request sent by curl; stdout and stderr lines.
#[cfg(unix)]
fn recorded_session(ws: &Path, upstream: &str) -> (Vec<String>, Vec<String>) {
    let args = [
        "--port",
        "0",
        "--upstream",
        upstream,
        "--save",
        "--json",
        "--har",
        "out/session.har",
    ];
    let mut child = spawn_record(ws, &args);
    let out = lines(child.stdout.take().unwrap());
    let err = lines(child.stderr.take().unwrap());
    let banner = next(&err);
    let addr = banner
        .strip_prefix("Recording reverse proxy on http://")
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("{banner}"))
        .to_owned();
    let output = curl()
        .unwrap()
        .args([
            "-sS",
            "--noproxy",
            "*",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
        ])
        .args(["-H", &format!("Authorization: Bearer {BEARER}")])
        .args(["-H", &format!("X-Api-Key: {X_API_KEY}")])
        .args(["-H", "Content-Type: application/json"])
        .args(["-d", &format!(r#"{{"user":"me","password":"{PASSWORD}"}}"#)])
        .arg(format!("http://{addr}/echo?api_key={API_KEY}"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "200");
    let mut stdout = vec![next(&out)];
    assert_eq!(interrupt(&mut child), Some(0));
    stdout.extend(out.iter());
    (stdout, err.iter().collect())
}

/// Task 8.1 through the binary and curl, reverse-proxy leg (the
/// interception leg is the test above): the request files, the
/// environment and the HAR hold no raw secret, `--json` keeps stdout to
/// JSON lines, and a second session with the same traffic changes
/// nothing.
#[cfg(unix)]
#[test]
fn curl_through_record_saves_request_files_and_resaves_without_a_diff() {
    if curl().is_none() {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let api = tls_api();
    let upstream = format!("https://localhost:{}", api.server.port());
    let ws = workspace_trusting(&api);
    let (stdout, stderr) = recorded_session(ws.path(), &upstream);
    assert_json_lines(&stdout);
    assert_saved(
        &stderr,
        "Saved 1 request file (0 unchanged), environment 'recorded'",
    );
    assert_first_save(ws.path(), &upstream, api.server.port());

    // The HAR is rewritten with new timings; the workspace files are not.
    let before = saved_files(ws.path());
    let (_, stderr) = recorded_session(ws.path(), &upstream);
    assert_saved(
        &stderr,
        "Saved 0 request files (1 unchanged), environment 'recorded'",
    );
    assert!(
        stderr.iter().all(|l| !l.contains("overwritten")),
        "{stderr:?}"
    );
    assert_eq!(
        saved_files(ws.path()),
        before,
        "the second session changes nothing"
    );
    assert_no_raw_secret(ws.path(), stdout.iter().chain(&stderr));
}

#[cfg(unix)]
fn assert_json_lines(stdout: &[String]) {
    for line in stdout {
        let value: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(value["type"], "exchange", "{line}");
    }
}

#[cfg(unix)]
fn assert_saved(stderr: &[String], summary: &str) {
    assert!(stderr.iter().any(|l| l == summary), "{stderr:?}");
}

/// The request file holds placeholders, the environment the base URL,
/// and the HAR redacted values.
#[cfg(unix)]
fn assert_first_save(ws: &Path, upstream: &str, port: u16) {
    let file =
        std::fs::read_to_string(ws.join(format!("recorded/localhost-{port}/post-echo.rman.yaml")))
            .unwrap();
    for placeholder in ["Bearer {{auth_token}}", "{{api_key}}", "{{password}}"] {
        assert!(file.contains(placeholder), "{placeholder}: {file}");
    }
    let env = std::fs::read_to_string(ws.join("environments/recorded.yaml")).unwrap();
    assert!(env.contains(&format!("base_url: {upstream}")), "{env}");
    let har = std::fs::read_to_string(ws.join("out/session.har")).unwrap();
    assert!(har.contains("«redacted"), "{har}");
}

/// The workspace files but the HAR folder.
#[cfg(unix)]
fn saved_files(ws: &Path) -> Vec<(String, Vec<u8>)> {
    contents(ws)
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("out/"))
        .collect()
}

/// No raw secret in any file of `ws` nor in the printed `lines`.
fn assert_no_raw_secret<'a>(ws: &Path, lines: impl Iterator<Item = &'a String>) {
    let secrets = [API_KEY, PASSWORD, X_API_KEY, BEARER];
    for (rel, bytes) in contents(ws) {
        let text = String::from_utf8_lossy(&bytes);
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} in {rel}");
        }
    }
    for line in lines {
        for secret in secrets {
            assert!(!line.contains(secret), "{secret} shown: {line}");
        }
    }
}

/// Task 8.1 with interception and the real OS keychain, as release builds
/// run it (CI runs the stand-in keychain version above). This test uses
/// the real keychain (never a trust store: curl is given the CA with `--cacert`) and
/// removes the CA afterwards. Run it by hand:
/// `cargo test -p rustman-cli --test record -- --ignored`.
#[cfg(unix)]
#[test]
#[ignore = "uses the OS keychain"]
fn curl_through_an_intercepting_recorder_with_the_os_keychain() {
    use rustman_core::secrets::keychain;
    use rustman_proxy::ca::store::CaStore;

    let api = tls_api();
    let ws = workspace_trusting(&api);
    let config = tempfile::tempdir().unwrap();
    let real = |args: &[&str]| {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"));
        cmd.current_dir(ws.path())
            .env_remove("RUSTMAN_SECRET_STORE")
            .env("XDG_CONFIG_HOME", config.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };
    let init = real(&["record", "ca", "init"]).output().unwrap();
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let id = std::fs::read_to_string(ws.path().join(".rustman/workspace-id")).unwrap();
    let id = id.trim().to_owned();
    let cert = config.path().join(format!("rustman/record-ca/{id}.pem"));
    let cleanup = || {
        let store = CaStore::new(keychain::os_keychain(), &config.path().join("rustman"), &id);
        store.remove().unwrap();
    };

    let mut child = real(&[
        "record",
        "--port",
        "0",
        "--intercept",
        "localhost",
        "--save",
        "--har",
        "out.har",
    ])
    .spawn()
    .unwrap();
    let out = lines(child.stdout.take().unwrap());
    let addr = banner_addr(&next(&out));
    next(&out);
    next(&out);
    let output = curl()
        .unwrap()
        .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}"])
        .args(["-x", &format!("http://{addr}")])
        .arg("--cacert")
        .arg(&cert)
        .args(["-H", &format!("Authorization: Bearer {BEARER}")])
        .args(["-H", &format!("X-Api-Key: {X_API_KEY}")])
        .args(["-H", "Content-Type: application/json"])
        .args(["-d", &format!(r#"{{"password":"{PASSWORD}"}}"#)])
        .arg(format!(
            "https://localhost:{}/echo?api_key={API_KEY}",
            api.server.port()
        ))
        .output()
        .unwrap();
    let code = interrupt(&mut child);
    cleanup();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "200", "{output:?}");
    assert_eq!(code, Some(0));
    let file = std::fs::read_to_string(ws.path().join(format!(
        "recorded/localhost-{}/post-echo.rman.yaml",
        api.server.port()
    )))
    .unwrap();
    assert!(file.contains("Bearer {{auth_token}}"), "{file}");
    assert!(ws.path().join("environments/recorded.yaml").is_file());
    assert!(ws.path().join("out.har").is_file());
    for (rel, bytes) in contents(ws.path()) {
        let text = String::from_utf8_lossy(&bytes);
        for secret in [API_KEY, PASSWORD, X_API_KEY, BEARER] {
            assert!(!text.contains(secret), "{secret} in {rel}");
        }
    }
}
