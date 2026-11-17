use super::*;
use crate::vars::{NoSecrets, Resolved, SecretSpan};

struct Store(Option<&'static str>);

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self
            .0
            .filter(|_| env == "dev" && name == "token")
            .map(str::to_owned))
    }
}

fn spanned(prefix: &str, value: &str) -> Resolved {
    Resolved {
        text: format!("{prefix}{value}"),
        secret_spans: vec![SecretSpan {
            name: "token".into(),
            range: prefix.len()..prefix.len() + value.len(),
        }],
    }
}

fn pair(name: &str, value: Resolved) -> ResolvedPair {
    ResolvedPair {
        name: Resolved::literal(name),
        value,
    }
}

fn request(authorization: Resolved) -> ResolvedRequest {
    ResolvedRequest {
        method: "POST".into(),
        url: Resolved::literal("http://h/x/{{not-a-var}}"),
        query: vec![pair("key", spanned("", "s3cr3t"))],
        headers: vec![
            pair("Authorization", authorization),
            pair("Accept", Resolved::literal("application/json")),
        ],
        body: ResolvedBody::Json(serde_json::json!({"token": "s3cr3t", "n": 1})),
        auth: Some(Auth::Bearer {
            token: "s3cr3t".into(),
        }),
        settings: RequestSettings {
            timeout_ms: Some(500),
            ..RequestSettings::default()
        },
        tls: TlsFiles::default(),
        secrets: vec![Secret::new("token", "s3cr3t".to_owned())],
    }
}

#[test]
fn stored_requests_hold_placeholders_not_secrets() {
    let replay = ReplayRequest::new(&request(spanned("Bearer ", "s3cr3t")), None);
    let json = serde_json::to_string(&replay).unwrap();
    assert!(!json.contains("s3cr3t"), "{json}");
    assert_eq!(replay.query[0].value, "{{token}}");
    assert_eq!(replay.headers[0].value, "Bearer {{token}}");
    assert_eq!(replay.url, "http://h/x/\\{{not-a-var}}");
    assert!(replay.withheld.is_empty());
}

#[test]
fn replay_resolves_the_current_secret_value() {
    let original = request(spanned("Bearer ", "s3cr3t"));
    let replay = ReplayRequest::new(&original, None);
    let stored: ReplayRequest =
        serde_json::from_str(&serde_json::to_string(&replay).unwrap()).unwrap();
    let again = stored.resolve(Some("dev"), &Store(Some("s3cr3t"))).unwrap();
    assert_eq!(again, original);
    let rotated = stored.resolve(Some("dev"), &Store(Some("n3w"))).unwrap();
    assert_eq!(rotated.headers[0].value.text, "Bearer n3w");
    assert_eq!(
        rotated.body,
        ResolvedBody::Json(serde_json::json!({"token": "n3w", "n": 1}))
    );
    assert_eq!(rotated.secrets[0].expose(), "n3w");
}

#[test]
fn a_deleted_secret_is_unresolved() {
    let replay = ReplayRequest::new(&request(spanned("Bearer ", "s3cr3t")), None);
    let err = replay.resolve(Some("dev"), &Store(None)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unresolved variable(s): token (secret not set; run 'rustman secret set token --env dev')"
    );
    let err = replay.resolve(None, &NoSecrets).unwrap_err();
    assert!(
        err.to_string().starts_with("unresolved variable(s): token"),
        "{err}"
    );
}

#[test]
fn literal_credentials_are_withheld() {
    let mut original = request(Resolved::literal("Bearer abc123"));
    original.auth = Some(Auth::Basic {
        username: "u".into(),
        password: "pw-literal".into(),
    });
    let replay = ReplayRequest::new(&original, None);
    let json = serde_json::to_string(&replay).unwrap();
    assert!(
        !json.contains("abc123") && !json.contains("pw-literal"),
        "{json}"
    );
    let err = replay
        .resolve(Some("dev"), &Store(Some("s3cr3t")))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "this exchange cannot be replayed: the Authorization header, the basic auth password \
         were not kept in history (send the request from its file instead)"
    );
}

#[test]
fn file_bodies_are_withheld() {
    let mut original = request(spanned("Bearer ", "s3cr3t"));
    original.body = ResolvedBody::File(ResolvedFile {
        raw: "a.bin".into(),
        path: PathBuf::from("/w/a.bin"),
    });
    let replay = ReplayRequest::new(&original, None);
    assert_eq!(replay.withheld, ["the multipart or file body"]);
}

#[test]
fn values_derived_from_a_secret_are_withheld() {
    let known = Redactor::new().with(&Secret::new("token", "s3cr3t".to_owned()));
    let mut clean = ReplayRequest::new(&request(spanned("Bearer ", "s3cr3t")), None);
    clean.withhold_derived(&known, &[]);
    assert!(clean.withheld.is_empty(), "{:?}", clean.withheld);

    let mut derived = request(spanned("Bearer ", "s3cr3t"));
    derived.url = Resolved {
        text: "http://h/use/x-s3cr3t".into(),
        secret_spans: vec![SecretSpan {
            name: "echoed".into(),
            range: 13..21,
        }],
    };
    let mut replay = ReplayRequest::new(&derived, None);
    replay.withhold_derived(&known, &["echoed".to_owned()]);
    assert_eq!(replay.url, "http://h/use/{{echoed}}");
    assert_eq!(replay.withheld, ["a value derived from a secret"]);
    let err = replay
        .resolve(Some("dev"), &Store(Some("s3cr3t")))
        .unwrap_err();
    assert!(err.to_string().contains("cannot be replayed"), "{err}");

    let mut leaking = request(spanned("Bearer ", "s3cr3t"));
    leaking.url = Resolved::literal("http://h/use/other-v4lue");
    let mut replay = ReplayRequest::new(&leaking, Some("key".into()));
    let other = known.with(&Secret::new("other", "other-v4lue".to_owned()));
    replay.withhold_derived(&other, &[]);
    let json = serde_json::to_string(&replay).unwrap();
    assert!(!json.contains("other-v4lue"), "{json}");
    assert_eq!(replay.withheld, ["a value derived from a secret"]);
    assert_eq!(replay.auth_cache_key.as_deref(), Some("key"));
}
