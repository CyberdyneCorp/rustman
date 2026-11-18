use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::secret::Secret;

struct Fixture {
    _tmp: tempfile::TempDir,
    request_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let request_file = tmp.path().join("users").join("get-user.rman.yaml");
        fs::create_dir_all(request_file.parent().unwrap()).unwrap();
        Self {
            _tmp: tmp,
            request_file,
        }
    }

    fn check(
        &self,
        spec: &SnapshotAssertion,
        exchange: &Exchange<'_>,
        mode: SnapshotMode,
        redactor: &Redactor,
    ) -> SnapshotOutcome {
        let ctx = SnapshotContext {
            request_file: &self.request_file,
            request: "users/get-user.rman.yaml",
            mode,
            redactor,
        };
        check(spec, exchange, &ctx).unwrap()
    }

    fn run(
        &self,
        spec: &SnapshotAssertion,
        exchange: &Exchange<'_>,
        mode: SnapshotMode,
    ) -> SnapshotOutcome {
        self.check(spec, exchange, mode, &Redactor::new())
    }

    fn file(&self) -> PathBuf {
        self.request_file
            .parent()
            .unwrap()
            .join("__snapshots__")
            .join("get-user.default.snap.yaml")
    }
}

fn headers(content_type: &str, date: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_owned(), content_type.to_owned()),
        ("Date".to_owned(), date.to_owned()),
    ]
}

fn exchange<'a>(headers: &'a [(String, String)], body: &'a str) -> Exchange<'a> {
    Exchange {
        kind: "http",
        status: Some(200),
        headers,
        body: body.as_bytes(),
        events: None,
    }
}

fn spec(text: &str) -> SnapshotAssertion {
    let checked: crate::model::Checked<SnapshotAssertion> =
        crate::format::from_yaml(std::path::Path::new("s.yaml"), text).unwrap();
    checked.into_inner()
}

#[test]
fn first_run_records_then_matches() {
    let fx = Fixture::new();
    let h = headers("application/json", "Mon, 16 Nov 2026 10:00:00 GMT");
    let first = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&h, r#"{"b":1,"a":"x"}"#),
        SnapshotMode::Record,
    );
    assert_eq!(first.state, SnapshotState::Recorded);
    assert_eq!(
        first.display_path,
        "users/__snapshots__/get-user.default.snap.yaml"
    );
    assert_eq!(first.path, fx.file());
    assert_eq!(
        fs::read_to_string(fx.file()).unwrap(),
        "version: 1\nrequest: users/get-user.rman.yaml\nname: default\nkind: http\ncompare:\n  - status\n  - body\nstatus: 200\nbody:\n  json:\n    a: x\n    b: 1\n"
    );
    let again = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&h, r#"{"a":"x","b":1}"#),
        SnapshotMode::Record,
    );
    assert_eq!(again.state, SnapshotState::Matched);
    assert!(again.state.passed() && again.diff.is_none());
}

#[test]
fn reordered_keys_give_byte_identical_files() {
    let a = Fixture::new();
    let b = Fixture::new();
    let h = headers("application/json", "x");
    a.run(
        &SnapshotAssertion::default(),
        &exchange(&h, r#"{"z":{"q":1,"p":[2,{"y":1,"x":0}]},"a":null}"#),
        SnapshotMode::Record,
    );
    b.run(
        &SnapshotAssertion::default(),
        &exchange(&h, r#"{"a":null,"z":{"p":[2,{"x":0,"y":1}],"q":1}}"#),
        SnapshotMode::Record,
    );
    assert_eq!(fs::read(a.file()).unwrap(), fs::read(b.file()).unwrap());
}

#[test]
fn date_header_change_is_ignored_by_default() {
    let fx = Fixture::new();
    let s = spec("compare: [status, headers, body]\n");
    let monday = headers("application/json", "Mon, 16 Nov 2026 10:00:00 GMT");
    let tuesday = headers("application/json", "Tue, 17 Nov 2026 10:00:00 GMT");
    fx.run(&s, &exchange(&monday, "{}"), SnapshotMode::Record);
    let later = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&tuesday, "{}"),
        SnapshotMode::Ci,
    );
    assert_ne!(
        later.state,
        SnapshotState::Matched,
        "compare list differs from the stored one"
    );
    let fx = Fixture::new();
    fx.run(
        &SnapshotAssertion::default(),
        &exchange(&monday, "{}"),
        SnapshotMode::Record,
    );
    let later = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&tuesday, "{}"),
        SnapshotMode::Ci,
    );
    assert_eq!(later.state, SnapshotState::Matched);
    let fx = Fixture::new();
    fx.run(&s, &exchange(&monday, "{}"), SnapshotMode::Record);
    assert_eq!(
        fx.run(&s, &exchange(&tuesday, "{}"), SnapshotMode::Ci)
            .state,
        SnapshotState::Matched
    );
}

#[test]
fn content_type_change_fails_with_the_header_diff() {
    let fx = Fixture::new();
    let s = spec("compare: [status, headers, body]\n");
    let json_h = headers("application/json", "x");
    let html_h = headers("text/html", "x");
    fx.run(&s, &exchange(&json_h, "{}"), SnapshotMode::Record);
    let failed = fx.run(&s, &exchange(&html_h, "{}"), SnapshotMode::Record);
    assert_eq!(failed.state, SnapshotState::Failed);
    assert!(
        failed
            .message
            .contains("changed headers.content-type: \"application/json\" → \"text/html\""),
        "{}",
        failed.message
    );
}

#[test]
fn ci_mode_never_writes() {
    let fx = Fixture::new();
    let h = headers("application/json", "x");
    let missing = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&h, "{}"),
        SnapshotMode::Ci,
    );
    assert_eq!(missing.state, SnapshotState::Missing);
    assert!(!missing.state.passed());
    assert_eq!(
        missing.message,
        "snapshot missing: users/__snapshots__/get-user.default.snap.yaml (run without --ci to record)"
    );
    assert!(!fx.file().exists());
    assert!(!fx.file().parent().unwrap().exists());
}

#[test]
fn update_mode_rewrites_only_differing_snapshots() {
    let fx = Fixture::new();
    let h = headers("application/json", "x");
    let s = SnapshotAssertion::default();
    fx.run(
        &s,
        &exchange(&h, r#"{"user":{"role":"admin"}}"#),
        SnapshotMode::Record,
    );
    let failed = fx.run(
        &s,
        &exchange(&h, r#"{"user":{"role":"viewer"}}"#),
        SnapshotMode::Record,
    );
    assert_eq!(failed.state, SnapshotState::Failed);
    assert_eq!(failed.message, r#"changed $.user.role: "admin" → "viewer""#);
    assert!(
        fs::read_to_string(fx.file())
            .unwrap()
            .contains("role: admin")
    );

    let updated = fx.run(
        &s,
        &exchange(&h, r#"{"user":{"role":"viewer"}}"#),
        SnapshotMode::Update,
    );
    assert_eq!(updated.state, SnapshotState::Updated);
    assert!(updated.state.passed() && updated.diff.is_some());
    assert!(
        fs::read_to_string(fx.file())
            .unwrap()
            .contains("role: viewer")
    );

    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(fx.file())
        .unwrap()
        .set_modified(past)
        .unwrap();
    let before = fs::metadata(fx.file()).unwrap().modified().unwrap();
    let same = fx.run(
        &s,
        &exchange(&h, r#"{"user":{"role":"viewer"}}"#),
        SnapshotMode::Update,
    );
    assert_eq!(same.state, SnapshotState::Matched);
    assert_eq!(fs::metadata(fx.file()).unwrap().modified().unwrap(), before);
}

#[test]
fn crlf_checkouts_still_match() {
    let fx = Fixture::new();
    let h = headers("application/json", "x");
    let s = SnapshotAssertion::default();
    fx.run(&s, &exchange(&h, r#"{"a":"b"}"#), SnapshotMode::Record);
    let crlf = fs::read_to_string(fx.file()).unwrap().replace('\n', "\r\n");
    fs::write(fx.file(), &crlf).unwrap();
    let outcome = fx.run(&s, &exchange(&h, r#"{"a":"b"}"#), SnapshotMode::Update);
    assert_eq!(outcome.state, SnapshotState::Matched);
    assert_eq!(fs::read_to_string(fx.file()).unwrap(), crlf);
}

#[test]
fn unreadable_snapshots_error_unless_updating() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.file().parent().unwrap()).unwrap();
    fs::write(fx.file(), "a: [\n").unwrap();
    let h = headers("application/json", "x");
    let redactor = Redactor::new();
    let ctx = SnapshotContext {
        request_file: &fx.request_file,
        request: "users/get-user.rman.yaml",
        mode: SnapshotMode::Record,
        redactor: &redactor,
    };
    let err = check(&SnapshotAssertion::default(), &exchange(&h, "{}"), &ctx).unwrap_err();
    assert!(matches!(err, SnapshotError::Invalid(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.starts_with("users/__snapshots__/get-user.default.snap.yaml:"),
        "{message}"
    );
    assert!(
        !message.contains(&*fx._tmp.path().to_string_lossy()),
        "{message}"
    );
    let updated = fx.run(
        &SnapshotAssertion::default(),
        &exchange(&h, "{}"),
        SnapshotMode::Update,
    );
    assert_eq!(updated.state, SnapshotState::Updated);
}

#[test]
fn echoed_token_is_redacted_in_the_file() {
    let fx = Fixture::new();
    let h = headers("application/json", "x");
    let redactor = Redactor::new().with(&Secret::new("token", "abc123".to_owned()));
    let body = r#"{"token":"abc123"}"#;
    fx.check(
        &SnapshotAssertion::default(),
        &exchange(&h, body),
        SnapshotMode::Record,
        &redactor,
    );
    let text = fs::read_to_string(fx.file()).unwrap();
    assert!(text.contains("«redacted:token»"), "{text}");
    assert!(!text.contains("abc123"), "{text}");
    let changed = r#"{"token":"abc123","n":1}"#;
    fx.check(
        &SnapshotAssertion::default(),
        &exchange(&h, changed),
        SnapshotMode::Update,
        &redactor,
    );
    assert!(!fs::read_to_string(fx.file()).unwrap().contains("abc123"));
}

#[test]
fn a_rule_that_rebuilds_a_secret_is_refused() {
    let fx = Fixture::new();
    let h = headers("text/plain", "x");
    let redactor = Redactor::new().with(&Secret::new("token", "abc123".to_owned()));
    let s = spec("replace: [{pattern: 'xyz', with: 'abc123'}]\n");
    let ctx = SnapshotContext {
        request_file: &fx.request_file,
        request: "users/get-user.rman.yaml",
        mode: SnapshotMode::Record,
        redactor: &redactor,
    };
    let err = check(&s, &exchange(&h, "xyz"), &ctx).unwrap_err();
    assert!(matches!(err, SnapshotError::Secret(_)));
    assert!(!err.to_string().contains("abc123"));
    assert!(!fx.file().exists());
}

#[test]
fn image_change_reports_size_and_hash() {
    let fx = Fixture::new();
    let h = vec![("content-type".to_owned(), "image/png".to_owned())];
    let old = vec![1_u8; 10240];
    let new = vec![2_u8; 10301];
    let first = Exchange {
        headers: &h,
        body: &old,
        ..exchange(&h, "")
    };
    fx.run(&SnapshotAssertion::default(), &first, SnapshotMode::Record);
    assert!(
        fs::read_to_string(fx.file())
            .unwrap()
            .contains("size: 10240")
    );
    let second = Exchange {
        body: &new,
        ..first
    };
    let failed = fx.run(&SnapshotAssertion::default(), &second, SnapshotMode::Record);
    let (a, b) = (sha256_hex(&old), sha256_hex(&new));
    assert_eq!(
        failed.message,
        format!(
            "binary body changed: 10240 B sha256:{}… → 10301 B sha256:{}…",
            &a[..4],
            &b[..4]
        )
    );
}

#[test]
fn event_count_change_is_reported() {
    let fx = Fixture::new();
    let three = [json!({"n": 1}), json!({"n": 2}), json!({"n": 3})];
    let first = Exchange {
        kind: "graphql",
        events: Some(&three),
        ..Exchange::default()
    };
    fx.run(&SnapshotAssertion::default(), &first, SnapshotMode::Record);
    let second = Exchange {
        events: Some(&three[..2]),
        ..first
    };
    let failed = fx.run(&SnapshotAssertion::default(), &second, SnapshotMode::Record);
    assert_eq!(failed.state, SnapshotState::Failed);
    assert!(
        failed.message.starts_with("event count changed: 3 → 2\n"),
        "{}",
        failed.message
    );
    assert!(
        failed.message.contains("removed $[2]"),
        "{}",
        failed.message
    );
}
