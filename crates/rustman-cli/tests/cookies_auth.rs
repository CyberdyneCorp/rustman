//! `rustman cookies clear` (task 4.7) and `rustman auth login` (task 4.9).
//!
//! Secrets use `RUSTMAN_SECRET_STORE=file`, so the OS keychain is never
//! touched.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread::JoinHandle;

use assert_cmd::Command;
use predicates::prelude::*;

fn rustman(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rustman").expect("rustman binary is built");
    cmd.current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV");
    cmd
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(tmp.path(), "environments/staging.yaml", "variables: {}\n");
    write(tmp.path(), "environments/prod.yaml", "variables: {}\n");
    tmp
}

#[test]
fn cookies_clear_empties_one_environment() {
    let ws = workspace();
    write(ws.path(), ".rustman/cookies/staging.json", "[]");
    write(ws.path(), ".rustman/cookies/prod.json", "[]");
    rustman(ws.path())
        .args(["cookies", "clear", "--env", "staging"])
        .assert()
        .success()
        .stdout("cleared cookies for environment 'staging'\n");
    assert!(!ws.path().join(".rustman/cookies/staging.json").exists());
    assert!(ws.path().join(".rustman/cookies/prod.json").exists());
    rustman(ws.path())
        .args(["cookies", "clear"])
        .env("RUSTMAN_ENV", "staging")
        .assert()
        .success()
        .stdout("no cookies stored for environment 'staging'\n");
}

#[test]
fn cookies_clear_without_an_environment_uses_the_shared_jar() {
    let ws = workspace();
    write(ws.path(), ".rustman/cookies/@none.json", "[]");
    rustman(ws.path())
        .args(["cookies", "clear"])
        .assert()
        .success()
        .stdout("cleared cookies for no environment\n");
}

#[test]
fn cookies_clear_rejects_an_unknown_environment() {
    let ws = workspace();
    rustman(ws.path())
        .args(["cookies", "clear", "--env", "qa"])
        .assert()
        .code(2)
        .stderr(predicate::str::starts_with(
            "error: environment 'qa' not found",
        ));
}

/// A token endpoint that answers one request with `body`, returning the
/// form it received.
fn token_server(body: &'static str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/token",
        listener.local_addr().unwrap().port()
    );
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut form = vec![0; length];
        reader.read_exact(&mut form).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        reader.get_mut().write_all(response.as_bytes()).unwrap();
        String::from_utf8(form).unwrap()
    });
    (url, handle)
}

#[test]
fn auth_login_stores_the_token_in_the_secret_store() {
    let ws = workspace();
    let (url, server) = token_server(r#"{"access_token":"cli-token","expires_in":3600}"#);
    let request = format!(
        "version: 1\nkind: http\nname: Me\nurl: http://127.0.0.1:1/me\n\
         auth: {{type: oauth2, grant: client_credentials, token_url: '{url}', client_id: app}}\n"
    );
    write(ws.path(), "api/me.rman.yaml", &request);
    rustman(ws.path())
        .args(["auth", "login", "Me", "--env", "prod"])
        .assert()
        .success()
        .stdout("logged in: token for 'api/me' stored for environment 'prod'\n");
    let form = server.join().unwrap();
    assert!(form.contains("grant_type=client_credentials"), "{form}");
    let secrets = fs::read_to_string(ws.path().join(".rustman/secrets.yaml")).unwrap();
    assert!(secrets.contains("oauth2:api/me"), "{secrets}");
    assert!(secrets.contains("cli-token"));
}

#[test]
fn auth_login_needs_oauth2_auth() {
    let ws = workspace();
    write(
        ws.path(),
        "api/me.rman.yaml",
        "version: 1\nkind: http\nname: Me\nurl: http://127.0.0.1:1/me\n",
    );
    rustman(ws.path())
        .args(["auth", "login", "api/me"])
        .assert()
        .code(3)
        .stderr("error: oauth2: the request does not use oauth2 auth\n");
    rustman(ws.path())
        .args(["auth", "login", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::starts_with(
            "error: request 'nope' not found",
        ));
}
