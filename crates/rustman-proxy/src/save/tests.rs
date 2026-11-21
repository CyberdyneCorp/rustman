use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use rustman_core::{Secret, SecretError};

use super::*;
use crate::capture::{BodyCapture, Recorded};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2lnbmF0dXJl";

fn exchange(method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> Capture {
    Capture {
        id: 1,
        started: SystemTime::UNIX_EPOCH,
        recorded: Recorded::Exchange(Box::new(Exchange {
            method: method.into(),
            url: url.into(),
            version: "HTTP/1.1".into(),
            request_headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            request_body: BodyCapture {
                bytes: body.as_bytes().to_vec(),
                total: body.len() as u64,
                truncated: false,
            },
            status: Some(200),
            response_headers: Vec::new(),
            response_body: BodyCapture::default(),
            duration: Duration::ZERO,
            intercepted: false,
            error: None,
        })),
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"rec\"\nversion = 1\n",
    )
    .unwrap();
    dir
}

fn options<'a>(root: &'a Path, environment: &str) -> SaveOptions<'a> {
    SaveOptions {
        root,
        into: None,
        environment: environment.to_owned(),
    }
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Every file under `dir` with its contents, sorted by path.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn captures_with_the_same_templated_path_make_one_file() {
    let ws = workspace();
    let captures = [
        exchange(
            "GET",
            "https://api.example.com/users/17",
            &[("Accept", "*/*")],
            "",
        ),
        exchange(
            "GET",
            "https://api.example.com/users/42",
            &[("Accept", "*/*")],
            "",
        ),
    ];
    let saved = save(&captures, &options(ws.path(), "recorded"), None).unwrap();
    assert_eq!((saved.written, saved.unchanged), (1, 0));
    let file = read(ws.path(), "recorded/api-example-com/get-users-id.rman.yaml");
    assert!(
        file.contains("url: '{{base_url}}/users/{{user_id}}'")
            || file.contains("url: \"{{base_url}}/users/{{user_id}}\""),
        "{file}"
    );
    let requests: Vec<String> = tree(ws.path())
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| p.ends_with(".rman.yaml"))
        .collect();
    assert_eq!(
        requests,
        ["recorded/api-example-com/get-users-id.rman.yaml"]
    );

    let before = tree(ws.path());
    let again = save(&captures, &options(ws.path(), "recorded"), None).unwrap();
    assert_eq!((again.written, again.unchanged), (0, 1));
    assert_eq!(tree(ws.path()), before, "re-saving changes nothing");
}

#[test]
fn the_environment_gets_the_base_urls_and_path_defaults() {
    let ws = workspace();
    let captures = [
        exchange("GET", "https://api.example.com/users/17", &[], ""),
        exchange("GET", "https://api.example.com/users/42", &[], ""),
        exchange("POST", "http://127.0.0.1:9000/orders", &[], ""),
    ];
    let saved = save(&captures, &options(ws.path(), "staging"), None).unwrap();
    let env = read(ws.path(), "environments/staging.yaml");
    assert!(env.contains("base_url: https://api.example.com"), "{env}");
    assert!(env.contains("base_url_2: http://127.0.0.1:9000"), "{env}");
    assert!(
        env.contains("user_id: '17'") || env.contains("user_id: \"17\""),
        "{env}"
    );
    assert_eq!(
        saved.warnings,
        [
            "warning: captures from http://127.0.0.1:9000 use {{base_url_2}} (more than one origin saved)"
        ]
    );
    let order = read(ws.path(), "recorded/127-0-0-1-9000/post-orders.rman.yaml");
    assert!(order.contains("{{base_url_2}}/orders"), "{order}");
}

#[test]
fn into_puts_every_request_in_one_folder() {
    let ws = workspace();
    let captures = [exchange(
        "DELETE",
        "https://api.example.com/items/9?force=1",
        &[],
        "",
    )];
    let options = SaveOptions {
        into: Some("api/items".to_owned()),
        ..options(ws.path(), "recorded")
    };
    save(&captures, &options, None).unwrap();
    let file = read(ws.path(), "api/items/delete-items-id.rman.yaml");
    assert!(file.contains("force"), "{file}");
}

#[test]
fn raw_secrets_never_reach_the_workspace() {
    let ws = workspace();
    let auth = format!("Bearer {TOKEN}");
    let captures = [exchange(
        "POST",
        "https://api.example.com/login?api_key=k3y-v4lu3-123456",
        &[
            ("Authorization", &auth),
            ("Cookie", "sid=s3ss10n-abcdef"),
            ("Content-Type", "application/json"),
            ("X-Trace", "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1"),
        ],
        r#"{"user":"me","password":"hunter2-pw"}"#,
    )];
    let saved = save(&captures, &options(ws.path(), "recorded"), None).unwrap();
    let file = read(ws.path(), "recorded/api-example-com/post-login.rman.yaml");
    assert!(file.contains("Bearer {{auth_token}}"), "{file}");
    let env = read(ws.path(), "environments/recorded.yaml");
    assert!(env.contains("- auth_token"), "declared secret: {env}");
    assert!(!saved.stored);
    assert!(
        saved.secrets.contains(&"auth_token".to_owned()),
        "{:?}",
        saved.secrets
    );
    assert!(
        saved.warnings.iter().any(|w| w.contains("'X-Trace'")),
        "{:?}",
        saved.warnings
    );
    for (path, bytes) in tree(ws.path()) {
        let text = String::from_utf8_lossy(&bytes);
        for raw in ["eyJ", "s3ss10n-abcdef", "hunter2-pw", "k3y-v4lu3-123456"] {
            assert!(!text.contains(raw), "{raw} in {path}: {text}");
        }
    }
}

#[test]
fn store_secrets_hands_the_values_to_the_store() {
    let ws = workspace();
    let auth = format!("Bearer {TOKEN}");
    let captures = [exchange(
        "GET",
        "https://api.example.com/me",
        &[("Authorization", &auth)],
        "",
    )];
    let kept: Mutex<Vec<(String, String, String)>> = Mutex::default();
    let store = |batch: &[(&str, &Secret<String>)]| -> Result<(), SecretError> {
        let mut kept = kept.lock().unwrap();
        for (env, secret) in batch {
            kept.push((
                (*env).to_owned(),
                secret.name().to_owned(),
                secret.expose().clone(),
            ));
        }
        Ok(())
    };
    let saved = save(&captures, &options(ws.path(), "recorded"), Some(&store)).unwrap();
    assert!(saved.stored);
    assert_eq!(
        kept.into_inner().unwrap(),
        [(
            "recorded".to_owned(),
            "auth_token".to_owned(),
            TOKEN.to_owned()
        )]
    );
    for (path, bytes) in tree(ws.path()) {
        assert!(!String::from_utf8_lossy(&bytes).contains("eyJ"), "{path}");
    }
}

#[test]
fn truncated_bodies_are_not_saved() {
    let mut capture = exchange("POST", "https://api.example.com/upload", &[], "partial");
    if let Recorded::Exchange(e) = &mut capture.recorded {
        e.request_body.truncated = true;
    }
    let exchanges: Vec<&Exchange> = [&capture].iter().filter_map(|c| c.exchange()).collect();
    let recording = collection(&exchanges, "recorded", true);
    assert_eq!(
        recording.warnings,
        ["warning: the body of POST /upload was truncated when recorded and is not saved"]
    );
}

#[test]
fn requests_without_a_response_are_saved_with_a_warning() {
    let mut capture = exchange("GET", "https://api.example.com/h2", &[], "");
    if let Recorded::Exchange(e) = &mut capture.recorded {
        e.status = None;
        e.error = Some("upstream error: tls".into());
    }
    let exchanges: Vec<&Exchange> = [&capture].iter().filter_map(|c| c.exchange()).collect();
    let recording = collection(&exchanges, "recorded", true);
    assert_eq!(
        recording.warnings,
        ["warning: GET /h2 got no response when recorded; its request is saved anyway"]
    );
}

#[test]
fn a_session_without_requests_writes_nothing() {
    let ws = workspace();
    let before = tree(ws.path());
    let tunnel = Capture {
        id: 1,
        started: SystemTime::UNIX_EPOCH,
        recorded: Recorded::Tunnel(crate::capture::Tunnel {
            host: "bank.example".into(),
            port: 443,
            bytes_up: 10,
            bytes_down: 20,
            duration: Duration::ZERO,
            error: None,
        }),
    };
    for captures in [Vec::new(), vec![tunnel]] {
        let saved = save(&captures, &options(ws.path(), "recorded"), None).unwrap();
        assert!(saved.is_nothing());
        assert_eq!((saved.written, saved.unchanged), (0, 0));
        assert_eq!(tree(ws.path()), before, "no environment, no folder");
        assert!(!ws.path().join("environments/recorded.yaml").exists());
    }
}

#[test]
fn an_invalid_environment_name_is_refused() {
    let ws = workspace();
    let err = save(&[] as &[Capture], &options(ws.path(), "My Env"), None).unwrap_err();
    assert!(matches!(err, SaveError::Environment(_)), "{err}");
}

/// A login with secrets in the query and the body, as two sessions send it.
fn login(api_key: &str, password: &str) -> Capture {
    exchange(
        "POST",
        &format!("http://localhost:18081/login?api_key={api_key}"),
        &[("Content-Type", "application/json")],
        &format!(r#"{{"user":"me","password":"{password}"}}"#),
    )
}

#[test]
fn a_later_session_reuses_the_secret_names_of_the_environment() {
    let ws = workspace();
    let first = save(
        &[login("QKEYSECRET55", "hunter2-pw-1")],
        &options(ws.path(), "recorded"),
        None,
    )
    .unwrap();
    let mut names = first.secrets.clone();
    names.sort();
    assert_eq!(names, ["api_key", "password"]);
    let file = read(ws.path(), "recorded/localhost-18081/post-login.rman.yaml");
    assert!(file.contains("{{api_key}}"), "{file}");
    assert!(file.contains("{{password}}"), "{file}");
    let before = tree(ws.path());

    // The same captures in a new session, then new values for the same
    // fields: the names stay, and nothing changes on disk.
    for (key, password) in [
        ("QKEYSECRET55", "hunter2-pw-1"),
        ("OTHERKEY77", "pw-2-rotated"),
    ] {
        let again = save(
            &[login(key, password)],
            &options(ws.path(), "recorded"),
            None,
        )
        .unwrap();
        assert_eq!((again.written, again.unchanged), (0, 1), "{key}");
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
        assert_eq!(tree(ws.path()), before, "re-saving {key} changes nothing");
    }
}

#[test]
fn a_plain_variable_of_the_environment_keeps_its_name() {
    let ws = workspace();
    std::fs::create_dir_all(ws.path().join("environments")).unwrap();
    std::fs::write(
        ws.path().join("environments/recorded.yaml"),
        "variables:\n  api_key: public-demo\n",
    )
    .unwrap();
    let saved = save(
        &[login("QKEYSECRET55", "hunter2-pw-1")],
        &options(ws.path(), "recorded"),
        None,
    )
    .unwrap();
    assert!(
        saved.secrets.contains(&"api_key_2".to_owned()),
        "{:?}",
        saved.secrets
    );
    let env = read(ws.path(), "environments/recorded.yaml");
    assert!(env.contains("api_key: public-demo"), "{env}");
}

#[test]
fn saving_part_of_an_earlier_recording_keeps_its_order_and_base_urls() {
    let ws = workspace();
    let all = [
        exchange("GET", "http://127.0.0.1:18081/health", &[], ""),
        exchange("GET", "http://localhost:18081/big", &[], ""),
        exchange("GET", "http://localhost:18081/orders/7/items", &[], ""),
    ];
    save(&all, &options(ws.path(), "recorded"), None).unwrap();
    let before = tree(ws.path());
    let env = read(ws.path(), "environments/recorded.yaml");
    assert!(env.contains("base_url_2: http://localhost:18081"), "{env}");

    let part = save(&all[2..], &options(ws.path(), "recorded"), None).unwrap();
    assert_eq!((part.written, part.unchanged), (0, 1));
    assert!(
        part.warnings.iter().all(|w| !w.contains("overwritten")),
        "{:?}",
        part.warnings
    );
    assert_eq!(tree(ws.path()), before, "a partial save changes nothing");

    save(
        &[exchange("DELETE", "http://localhost:18081/big", &[], "")],
        &options(ws.path(), "recorded"),
        None,
    )
    .unwrap();
    let order = read(ws.path(), "recorded/localhost-18081/_collection.yaml");
    let entries: Vec<&str> = ["get-big", "get-orders-id-items", "delete-big"]
        .into_iter()
        .filter(|e| order.contains(e))
        .collect();
    assert_eq!(entries.len(), 3, "{order}");
    assert!(order.find("get-big") < order.find("delete-big"), "{order}");
    let root = read(ws.path(), "recorded/_collection.yaml");
    assert!(root.contains("127-0-0-1-18081"), "{root}");
    let delete = read(ws.path(), "recorded/localhost-18081/delete-big.rman.yaml");
    assert!(delete.contains("{{base_url_2}}/big"), "{delete}");
}

/// Secrets the save path finds by their value or shape rather than their
/// name: a token in a neutral-named header, well-known tokens in a JSON
/// body and the query, an auth scheme value in a body field.
fn shaped_secrets() -> (Capture, [&'static str; 5]) {
    let header_token = "ghp_q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
    let body_token = "ghp_Zx9Yw8Vu7Ts6Rq5Po4Nm3Lk2Ji1Hg0FeDcBa";
    let query_token = "ghp_Aa1Bb2Cc3Dd4Ee5Ff6Gg7Hh8Ii9Jj0KkLlMm";
    let aws_key = "AKIAIOSFODNN7EXAMPLE";
    let body = format!(r#"{{"data":"{body_token}","x":"{aws_key}","authorization":"Bearer X"}}"#);
    let capture = exchange(
        "POST",
        &format!("https://api.example.com/sync?state={query_token}"),
        &[
            ("Content-Type", "application/json"),
            ("X-Forward", header_token),
        ],
        &body,
    );
    let raw = [header_token, body_token, query_token, aws_key, "Bearer X"];
    (capture, raw)
}

#[test]
fn shown_captures_redact_every_secret_the_save_path_parameterizes() {
    let ws = workspace();
    let (capture, raw) = shaped_secrets();
    let captures = [capture];
    let saved = save(&captures, &options(ws.path(), "recorded"), None).unwrap();
    for (path, bytes) in tree(ws.path()) {
        let text = String::from_utf8_lossy(&bytes);
        for secret in raw {
            assert!(!text.contains(secret), "saved: {secret} in {path}: {text}");
        }
    }
    let har = crate::har::har(&captures, false).to_string();
    let json = crate::jsonl::capture_json(&captures[0]).to_string();
    let line = captures[0]
        .redactor()
        .text(&captures[0].to_string())
        .into_owned();
    for (shown, text) in [("HAR", &har), ("--json", &json), ("live line", &line)] {
        for secret in raw {
            assert!(!text.contains(secret), "{shown}: {secret} in {text}");
        }
    }
    assert!(
        saved.warnings.iter().all(|w| !w.contains("high entropy")),
        "a parameterized value is reported as kept: {:?}",
        saved.warnings
    );
}
