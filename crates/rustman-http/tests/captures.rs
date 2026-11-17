//! Assertions and captures across the requests of a collection (task 5.4):
//! token chaining, required captures, persisted captures across runs and
//! secret captures.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rustman_core::{Redactor, SecretStore};
use support::TestServer;
use support::collection::{Ran, run, workspace, write};
use support::routes::TOKEN;

/// Every file under `environments/`, with its bytes.
fn environment_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(root.join("environments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

const LOGIN: &str = "version: 1
kind: http
name: login
method: POST
url: '{{base_url}}/token'
assertions:
  - status: 200
  - json: {path: $.access_token, exists: true}
captures:
  - {var: token, from: json, path: $.access_token}
  - {var: user_id, from: json, path: $.user.id, required: true}
";

const ME: &str = "version: 1
kind: http
name: me
url: '{{base_url}}/me'
headers:
  - {name: Authorization, value: 'Bearer {{token}}'}
assertions:
  - status: 200
  - json: {path: $.id, equals: '{{user_id}}'}
";

#[tokio::test]
async fn token_chaining() {
    let server = TestServer::http().await;
    let dir = workspace(&server, &[]);
    write(
        dir.path(),
        &[
            ("auth/1-login.rman.yaml", LOGIN),
            ("auth/2-me.rman.yaml", ME),
        ],
    );
    let ran = run(dir.path(), "auth", "dev").await;
    let names: Vec<&str> = ran.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["login", "me"]);
    for r in &ran {
        assert!(
            r.passed(),
            "{}: {:?} {:?}",
            r.name,
            r.assertions,
            r.captures
        );
    }
    // Without the capture, `me` is unauthorized.
    let unchained = ME.replace("{{token}}", "wrong");
    write(dir.path(), &[("auth/2-me.rman.yaml", &unchained)]);
    let ran = run(dir.path(), "auth", "dev").await;
    assert_eq!(
        ran[1].assertions[0].message,
        "status: expected 200, got 401"
    );
}

#[tokio::test]
async fn a_missing_required_capture_fails_the_request() {
    let server = TestServer::http().await;
    let dir = workspace(&server, &[]);
    let request = "version: 1
kind: http
name: create
url: '{{base_url}}/token'
captures:
  - {var: id, from: json, path: $.id, required: true}
";
    write(dir.path(), &[("orders/create.rman.yaml", request)]);
    let ran = run(dir.path(), "orders", "dev").await;
    assert!(!ran[0].passed());
    assert_eq!(
        ran[0].captures.failures,
        ["capture 'id': no match for $.id"]
    );
}

#[tokio::test]
async fn persisted_captures_survive_runs_and_leave_environments_alone() {
    let server = TestServer::http().await;
    let dir = workspace(&server, &[]);
    let login = "version: 1
kind: http
name: login
method: POST
url: '{{base_url}}/login'
captures:
  - {var: session, from: cookie, name: sid, persist: true}
";
    let later = "version: 1
kind: http
name: later
url: '{{base_url}}/echo'
headers:
  - {name: X-Session, value: '{{session}}'}
assertions:
  - json: {path: \"$.headers[?@[0] == 'x-session'][1]\", equals: abc}
";
    write(
        dir.path(),
        &[
            ("first/login.rman.yaml", login),
            ("second/later.rman.yaml", later),
        ],
    );
    let before = environment_files(dir.path());
    let first = run(dir.path(), "first", "dev").await;
    assert!(first[0].passed() && first[0].captures.results[0].persisted);
    // A separate run: no captures in memory, only the state file.
    let second = run(dir.path(), "second", "dev").await;
    assert!(second[0].passed(), "{:?}", second[0].assertions);
    assert_eq!(
        fs::read_to_string(dir.path().join(".rustman/state/dev.yaml")).unwrap(),
        "session: abc\n"
    );
    assert_eq!(environment_files(dir.path()), before);
}

#[tokio::test]
async fn secret_captures_are_stored_as_secrets_and_redacted() {
    let server = TestServer::http().await;
    let dir = workspace(&server, &["token"]);
    write(
        dir.path(),
        &[
            ("auth/1-login.rman.yaml", LOGIN),
            ("auth/2-me.rman.yaml", ME),
        ],
    );
    let before = environment_files(dir.path());
    let ran = run(dir.path(), "auth", "dev").await;
    assert!(ran.iter().all(Ran::passed));
    let report = &ran[0].captures;
    assert!(report.results[0].secret && report.results[0].persisted);
    // The login response carried the token: a sink redacts it with the
    // report's secrets.
    let mut redactor = Redactor::new();
    for secret in &report.secrets {
        redactor.add(secret);
    }
    let body = format!("{{\"access_token\":\"{TOKEN}\"}}");
    assert_eq!(
        redactor.text(&body),
        "{\"access_token\":\"«redacted:token»\"}"
    );

    let store = SecretStore::with_keychain(dir.path(), None);
    let stored = store.get("dev", "token").unwrap().unwrap();
    assert_eq!(stored.expose(), TOKEN);
    assert!(!dir.path().join(".rustman/state").exists());
    assert_eq!(environment_files(dir.path()), before);
    // A later run resolves the secret from the store.
    write(
        dir.path(),
        &[(
            "later/me.rman.yaml",
            ME.replace("'{{user_id}}'", "42").as_str(),
        )],
    );
    let later = run(dir.path(), "later", "dev").await;
    assert!(later[0].passed(), "{:?}", later[0].assertions);
}
