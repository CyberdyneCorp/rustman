//! Secret provenance and the `Redactor` over a real workspace (task 3.5).
//!
//! A canary secret goes through the secret store and the resolver; every
//! output this crate can produce so far (Debug of resolved values, error
//! messages, redacted views of an echoed exchange) must not contain it.

use std::fs;
use std::path::Path;

use rustman_core::secrets::SecretBackend;
use rustman_core::vars::{ResolvedBody, ResolvedRequest, Resolver, resolve_request};
use rustman_core::{Redactor, Secret, SecretStore, Workspace};

const CANARY: &str = "canary-7f3a9c-s3cr3t";

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
    write(
        tmp.path(),
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\nsecrets: [api_token, password]\n",
    );
    write(
        tmp.path(),
        "api/echo.rman.yaml",
        r#"version: 1
kind: http
name: Echo
method: POST
url: '{{base_url}}/echo?key={{api_token}}'
headers:
  - name: X-Api-Key
    value: '{{api_token}}'
  - name: Authorization
    value: Bearer abc123
auth:
  type: basic
  username: me
  password: '{{password}}'
body:
  type: json
  content:
    token: '{{api_token}}'
    nested: ['{{api_token}}']
"#,
    );
    tmp
}

fn store(root: &Path) -> SecretStore {
    let store = SecretStore::open(root, SecretBackend::File);
    store
        .set("prod", &Secret::new("api_token", CANARY.to_owned()))
        .unwrap();
    store
        .set("prod", &Secret::new("password", "pw-canary".to_owned()))
        .unwrap();
    store
}

fn resolve(ws: &Workspace, store: &SecretStore) -> ResolvedRequest {
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let request = ws.find_by_name("Echo").remove(0);
    let resolver = Resolver::for_request(&ws.settings, &request, env, store);
    resolve_request(&request, &resolver).unwrap()
}

#[test]
fn transport_sees_real_values_with_provenance() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let resolved = resolve(&ws, &store(tmp.path()));
    assert_eq!(
        resolved.url.text,
        format!("https://api.example.com/echo?key={CANARY}")
    );
    let key = &resolved.headers[0].value;
    assert_eq!(key.text, CANARY);
    assert_eq!(key.secret_spans[0].name, "api_token");
    assert_eq!(key.secret_spans[0].range, 0..CANARY.len());
    let names: Vec<&str> = resolved.secrets.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["api_token", "password"]);
    let ResolvedBody::Json(body) = &resolved.body else {
        panic!("json body");
    };
    assert_eq!(body["token"], CANARY);
}

#[test]
fn debug_output_is_redacted() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let resolved = resolve(&ws, &store(tmp.path()));
    for debug in [format!("{resolved:?}"), format!("{resolved:#?}")] {
        assert!(!debug.contains(CANARY), "{debug}");
        assert!(!debug.contains("pw-canary"), "{debug}");
        assert!(debug.contains("«redacted:api_token»"), "{debug}");
        assert!(debug.contains("«redacted:password»"), "{debug}");
        // A literal sensitive header is redacted too.
        assert!(!debug.contains("abc123"), "{debug}");
        assert!(debug.contains("«redacted:Authorization»"), "{debug}");
    }
    let header = format!("{:?}", resolved.headers[0]);
    assert!(!header.contains(CANARY) && header.contains("«redacted:api_token»"));
    for field in [
        format!("{:?}", resolved.body),
        format!("{:#?}", resolved.auth),
        format!("{:?}", resolved.settings),
    ] {
        assert!(!field.contains(CANARY), "{field}");
        assert!(!field.contains("pw-canary"), "{field}");
    }
}

#[test]
fn debug_of_a_json_body_redacts_secrets_with_non_printable_characters() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let store = SecretStore::open(tmp.path(), SecretBackend::File);
    for (name, value) in [
        ("api_token", "a\u{7f}b-canary"),
        ("password", "c\u{200b}d-canary"),
    ] {
        store
            .set("prod", &Secret::new(name, value.to_owned()))
            .unwrap();
    }
    let resolved = resolve(&ws, &store);
    let debug = format!("{resolved:?}");
    assert!(!debug.contains("a\\u{7f}b"), "{debug}");
    assert!(!debug.contains("a\u{7f}b"), "{debug}");
    assert!(!debug.contains("c\u{200b}d"), "{debug}");
    assert!(debug.contains("«redacted:api_token»"), "{debug}");
    assert!(debug.contains("«redacted:password»"), "{debug}");
}

#[test]
fn echoed_secret_is_redacted_from_an_exchange() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let resolved = resolve(&ws, &store(tmp.path()));
    let redactor = Redactor::for_request(&resolved);
    // What a server echoing the request headers and query would send back.
    let echo = format!(
        r#"{{"headers":{{"X-Api-Key":"{CANARY}","Authorization":"Bearer abc123"}},"args":{{"key":"{CANARY}"}}}}"#
    );
    let shown = redactor.text(&echo);
    assert!(!shown.contains(CANARY), "{shown}");
    assert_eq!(shown.matches("«redacted:api_token»").count(), 2);
    assert_eq!(
        redactor.resolved(&resolved.url),
        "https://api.example.com/echo?key=«redacted:api_token»"
    );
    let headers: Vec<String> = resolved
        .headers
        .iter()
        .map(|h| {
            format!(
                "{}: {}",
                h.name.text,
                redactor.resolved_header_value(&h.name.text, &h.value)
            )
        })
        .collect();
    assert_eq!(
        headers,
        [
            "X-Api-Key: «redacted:api_token»",
            "Authorization: «redacted:Authorization»"
        ]
    );
}

#[test]
fn unresolved_secret_error_has_the_hint_and_no_value() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let store = store(tmp.path());
    store.remove("prod", "api_token").unwrap();
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let request = ws.find_by_name("Echo").remove(0);
    let resolver = Resolver::for_request(&ws.settings, &request, env, &store);
    let err = resolve_request(&request, &resolver)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "unresolved variable(s): api_token (secret not set; run 'rustman secret set api_token --env prod')"
    );
}

#[test]
fn no_workspace_file_holds_the_secret() {
    let tmp = workspace();
    let ws = Workspace::load(tmp.path()).unwrap();
    let _ = resolve(&ws, &store(tmp.path()));
    let mut stack = vec![tmp.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if !path.ends_with(".rustman/secrets.yaml") {
                let text = fs::read_to_string(&path).unwrap_or_default();
                assert!(!text.contains(CANARY), "{}", path.display());
            }
        }
    }
}

#[test]
fn debug_output_redacts_literal_credentials() {
    let tmp = workspace();
    write(
        tmp.path(),
        "api/literal.rman.yaml",
        "version: 1\nkind: http\nname: Literal\nurl: http://x\nheaders:\n  - {name: cookie, value: sid=lit-cookie}\nauth: {type: bearer, token: lit-token}\nsettings: {client_pkcs12_password: lit-pkcs}\n",
    );
    write(
        tmp.path(),
        "api/key.rman.yaml",
        "version: 1\nkind: http\nname: Key\nurl: http://x\nauth: {type: api_key, name: X-Key, value: lit-key}\n",
    );
    let ws = Workspace::load(tmp.path()).unwrap();
    let store = store(tmp.path());
    let env = ws.active_environment(Some("prod"), None).unwrap();
    for name in ["Literal", "Key"] {
        let request = ws.find_by_name(name).remove(0);
        let resolver = Resolver::for_request(&ws.settings, &request, env, &store);
        let resolved = resolve_request(&request, &resolver).unwrap();
        let debug = format!("{resolved:?} {:?} {:?}", resolved.auth, resolved.settings);
        for literal in ["lit-cookie", "lit-token", "lit-pkcs", "lit-key"] {
            assert!(!debug.contains(literal), "{debug}");
        }
    }
}
