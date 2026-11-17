use std::borrow::Cow;
use std::fs;
use std::path::Path;

use super::*;
use crate::model::{VarValue, Variables};

fn captures(yaml: &str) -> Vec<Checked<Capture>> {
    crate::format::from_yaml(Path::new("c.yaml"), yaml).unwrap()
}

fn response(body: &str) -> ResponseView<'static> {
    ResponseView {
        status: 200,
        headers: vec![("Set-Cookie".into(), "sid=abc123; Path=/".into())],
        body: Cow::Owned(body.as_bytes().to_vec()),
        ..ResponseView::default()
    }
}

fn environment(secrets: &[&str]) -> Environment {
    Environment {
        name: "dev".into(),
        variables: Variables::new(vec![("base_url".into(), VarValue::String("x".into()))]),
        secrets: secrets.iter().map(|s| (*s).to_owned()).collect(),
        production: false,
    }
}

#[test]
fn captured_values_join_the_run_scope() {
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: token, from: json, path: $.access_token}\n- {var: code, from: status}\n",
        ),
        &response(r#"{"access_token":"t-1"}"#),
        &mut scope,
        None,
    );
    assert!(!report.failed() && report.warnings.is_empty(), "{report:?}");
    assert_eq!(scope.get("token"), Some(&Lookup::Value("t-1".into())));
    assert_eq!((&scope).lookup("code"), Some(Lookup::Value("200".into())));
    assert_eq!((&scope).layer(), Layer::Capture);
    assert_eq!(report.results[0].outcome, CaptureOutcome::Captured);
}

#[test]
fn a_later_capture_replaces_an_earlier_one() {
    let mut scope = RunCaptures::new();
    scope.set("a", Lookup::Value("1".into()));
    scope.set("a", Lookup::Value("2".into()));
    assert_eq!(scope.get("a"), Some(&Lookup::Value("2".into())));
}

#[test]
fn a_required_capture_that_matches_nothing_fails_the_request() {
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures("- {var: id, from: json, path: $.id, required: true}\n"),
        &response(r#"{"name":"x"}"#),
        &mut scope,
        None,
    );
    assert_eq!(report.failures, ["capture 'id': no match for $.id"]);
    assert!(report.failed());
    assert!(scope.get("id").is_none());
    assert_eq!(report.results[0].outcome, CaptureOutcome::NoMatch);
}

#[test]
fn an_optional_capture_that_matches_nothing_only_warns() {
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: id, from: json, path: $.id}\n- {var: n, from: body_regex, pattern: '('}\n",
        ),
        &response("not json"),
        &mut scope,
        None,
    );
    assert!(!report.failed());
    assert_eq!(
        report.warnings[0],
        "capture 'id': response body is not JSON"
    );
    assert!(report.warnings[1].starts_with("capture 'n': invalid regular expression /(/"));
    assert_eq!(report.results[0].outcome, CaptureOutcome::Error);
    assert!(scope.is_empty());
}

#[test]
fn persisted_captures_go_to_the_state_file_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("environments")).unwrap();
    fs::write(
        root.join("environments/dev.yaml"),
        "variables:\n  base_url: x\n",
    )
    .unwrap();
    let env = environment(&[]);
    let store = SecretStore::with_keychain(root, None);
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &Redactor::new(),
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: session, from: cookie, name: sid, persist: true}\n- {var: temp, from: status}\n",
        ),
        &response(""),
        &mut scope,
        Some(&persistence),
    );
    assert!(report.warnings.is_empty(), "{report:?}");
    assert!(report.results[0].persisted && !report.results[1].persisted);

    let state = fs::read_to_string(root.join(".rustman/state/dev.yaml")).unwrap();
    assert_eq!(state, "session: abc123\n");
    let loaded = PersistedState::load(root, Some("dev")).unwrap();
    assert_eq!(
        loaded.source().lookup("session"),
        Some(Lookup::Value("abc123".into()))
    );
    assert_eq!(loaded.source().layer(), Layer::PersistedCapture);
    assert_eq!(
        fs::read_to_string(root.join("environments/dev.yaml")).unwrap(),
        "variables:\n  base_url: x\n"
    );
}

#[test]
fn state_updates_keep_other_entries_and_odd_values() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    PersistedState::persist(root, None, "b", "no").unwrap();
    PersistedState::persist(root, None, "a", "it's: {x}\n2").unwrap();
    PersistedState::persist(root, None, "b", "0755").unwrap();
    let loaded = PersistedState::load(root, None).unwrap();
    let pairs: Vec<(String, String)> = loaded
        .variables()
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_string()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("a".into(), "it's: {x}\n2".into()),
            ("b".into(), "0755".into())
        ]
    );
    assert!(root.join(".rustman/state/@none.yaml").is_file());
    assert_eq!(
        PersistedState::load(root, Some("other")).unwrap(),
        PersistedState::default()
    );
}

#[test]
fn secret_captures_go_to_the_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = environment(&["token"]);
    let store = SecretStore::with_keychain(root, None);
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &Redactor::new(),
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures("- {var: token, from: json, path: $.t, persist: true}\n"),
        &response(r#"{"t":"canary-tok"}"#),
        &mut scope,
        Some(&persistence),
    );
    assert!(report.warnings.is_empty(), "{report:?}");
    let result = &report.results[0];
    assert!(result.secret && result.persisted);
    assert!(!format!("{report:?}").contains("canary-tok"));
    assert_eq!(report.secrets[0].expose(), "canary-tok");
    assert!(matches!(scope.get("token"), Some(Lookup::Secret(s)) if s.expose() == "canary-tok"));
    assert_eq!(
        store
            .get("dev", "token")
            .unwrap()
            .map(|s| s.expose().clone()),
        Some("canary-tok".into())
    );
    assert!(!root.join(".rustman/state").exists());
}

#[test]
fn persisting_without_a_workspace_warns() {
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures("- {var: code, from: status, persist: true}\n"),
        &response(""),
        &mut scope,
        None,
    );
    assert_eq!(
        report.warnings,
        ["capture 'code': not persisted (no workspace)"]
    );
    assert!(scope.get("code").is_some());
}

#[test]
fn a_value_echoing_a_secret_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = environment(&["tok"]);
    let store = SecretStore::with_keychain(root, None);
    let redactor = Redactor::new().with(&Secret::new("tok", "Qz9-c4n4ry".to_owned()));
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &redactor,
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: plain, from: json, path: $.path, persist: true}\n- {var: ok, from: status, persist: true}\n",
        ),
        &response(r#"{"path":"/echo?k=Qz9-c4n4ry&n=1"}"#),
        &mut scope,
        Some(&persistence),
    );
    assert_eq!(
        report.warnings,
        ["capture 'plain': not persisted: the value contains a secret"]
    );
    assert!(!report.failed() && !report.results[0].persisted && report.results[1].persisted);
    assert!(scope.get("plain").is_some());
    let state = fs::read_to_string(root.join(".rustman/state/dev.yaml")).unwrap();
    assert_eq!(state, "ok: '200'\n");
}

#[test]
fn a_value_echoing_a_secret_captured_earlier_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = environment(&["token"]);
    let store = SecretStore::with_keychain(root, None);
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &Redactor::new(),
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: token, from: json, path: $.t}\n- {var: copy, from: json, path: $.c, persist: true}\n",
        ),
        &response(r#"{"t":"canary-tok","c":"Bearer canary-tok"}"#),
        &mut scope,
        Some(&persistence),
    );
    assert!(!report.results[1].persisted, "{report:?}");
    assert!(!root.join(".rustman/state").exists());
}

#[test]
fn an_invalid_json_path_is_named_in_the_error() {
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: foo, from: json, path: '$.<id>'}\n- {var: ok, from: json, path: $.id}\n",
        ),
        &response(r#"{"id":1}"#),
        &mut scope,
        None,
    );
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    assert!(
        report.warnings[0].starts_with("capture 'foo': invalid JSONPath $.<id>: "),
        "{:?}",
        report.warnings
    );
    assert_eq!(report.results[0].outcome, CaptureOutcome::Error);
}

#[test]
fn values_holding_a_known_secret_become_secrets() {
    let mut scope = RunCaptures::new();
    scope.set("echoed", Lookup::Value("x-canary-tok-y".into()));
    scope.set("plain", Lookup::Value("nothing".into()));
    let known = Redactor::new().with(&Secret::new("api_token", "canary-tok".to_owned()));
    let tainted = scope.taint(&known);
    assert_eq!(tainted.len(), 1);
    assert_eq!(tainted[0].name(), "echoed");
    assert_eq!(tainted[0].expose(), "x-canary-tok-y");
    assert!(matches!(scope.get("echoed"), Some(Lookup::Secret(_))));
    assert!(matches!(scope.get("plain"), Some(Lookup::Value(_))));
    assert_eq!(scope.derived(), ["echoed"]);
    let mut run = RunCaptures::new();
    run.merge(scope);
    assert_eq!(run.derived(), ["echoed"]);
    run.set("echoed", Lookup::Value("fresh".into()));
    assert!(run.derived().is_empty());
}

#[test]
fn a_piece_of_a_secret_is_not_persisted_and_becomes_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = environment(&[]);
    let store = SecretStore::with_keychain(root, None);
    let redactor = Redactor::new().with(&Secret::new("tok", "simpleCANARY".to_owned()));
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &redactor,
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures("- {var: part, from: body_regex, pattern: 'simpleCAN', persist: true}\n"),
        &response("echo simpleCANARY"),
        &mut scope,
        Some(&persistence),
    );
    assert_eq!(
        report.warnings,
        ["capture 'part': not persisted: the value contains a secret"]
    );
    assert!(!root.join(".rustman/state/dev.yaml").exists());
    assert_eq!(scope.taint(&redactor).len(), 1);
    assert!(matches!(scope.get("part"), Some(Lookup::Secret(_))));
}

#[test]
fn a_capture_straddling_or_masking_a_secret_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = environment(&[]);
    let store = SecretStore::with_keychain(root, None);
    let redactor = Redactor::new().with(&Secret::new("tok", "QJX7vwP3K".to_owned()));
    let persistence = Persistence {
        root,
        environment: Some(&env),
        secrets: &store,
        redactor: &redactor,
    };
    let mut scope = RunCaptures::new();
    let report = apply(
        &captures(
            "- {var: edge, from: body_regex, pattern: 'v.:.QJX7v', persist: true}\n\
             - {var: masked, from: body_regex, pattern: '[*]+wP3K', persist: true}\n",
        ),
        &response(r#"{"v":"QJX7vwP3K","m":"*****wP3K"}"#),
        &mut scope,
        Some(&persistence),
    );
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(!root.join(".rustman/state/dev.yaml").exists());
    assert_eq!(scope.taint(&redactor).len(), 2);
}
