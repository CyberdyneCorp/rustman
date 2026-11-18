use proptest::prelude::*;
use serde_json::json;

use super::*;
use crate::model::{Replacement, SortRule};
use crate::secret::Secret;
use crate::snapshot::EPOCH_MS;

fn spec(text: &str) -> SnapshotAssertion {
    let checked: crate::model::Checked<SnapshotAssertion> =
        crate::format::from_yaml(std::path::Path::new("s.yaml"), text).unwrap();
    checked.into_inner()
}

fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".into(), "application/json".into())]
}

fn body_of(spec: &SnapshotAssertion, redactor: &Redactor, body: &str) -> NormalizedBody {
    let headers = json_headers();
    let exchange = Exchange {
        kind: "http",
        status: Some(200),
        headers: &headers,
        body: body.as_bytes(),
        events: None,
    };
    let normalizer = Normalizer::new(spec, redactor).unwrap();
    normalizer.normalize(&exchange).body.unwrap()
}

fn json_of(spec: &SnapshotAssertion, body: &str) -> Value {
    match body_of(spec, &Redactor::new(), body) {
        NormalizedBody::Json(value) => value,
        other => panic!("not JSON: {other:?}"),
    }
}

#[test]
fn secrets_are_redacted_first() {
    let redactor = Redactor::new().with(&Secret::new("token", "abc123".to_owned()));
    let body = r#"{"token":"abc123","abc123":1,"pin":"x-abc123-y"}"#;
    let NormalizedBody::Json(value) = body_of(&SnapshotAssertion::default(), &redactor, body)
    else {
        panic!("not JSON");
    };
    let text = value.to_string();
    assert!(!text.contains("abc123"), "{text}");
    assert_eq!(value["token"], "«redacted:token»");
    assert_eq!(value["pin"], "x-«redacted:token»-y");
    assert!(value.get("«redacted:token»").is_some());

    let number = Redactor::new().with(&Secret::new("pin", "987654".to_owned()));
    let NormalizedBody::Json(value) =
        body_of(&SnapshotAssertion::default(), &number, r#"{"pin":987654}"#)
    else {
        panic!("not JSON");
    };
    assert_eq!(value["pin"], "«redacted:pin»");
}

#[test]
fn redaction_runs_before_ignore_and_replace() {
    let redactor = Redactor::new().with(&Secret::new("token", "abc123".to_owned()));
    let rules = spec("replace: [{pattern: 'redacted', with: 'x'}]\n");
    let NormalizedBody::Text(text) = body_of(&rules, &redactor, "plain abc123") else {
        panic!("not text");
    };
    assert_eq!(
        text, "plain «x:token»",
        "replace sees the placeholder, never the value"
    );
}

#[test]
fn sensitive_and_listed_headers_are_redacted() {
    let s = spec(
        "compare: [headers]\nheaders: {include: [authorization, x-key, content-type], redact: [x-key]}\n",
    );
    let headers = vec![
        ("Authorization".to_owned(), "Bearer abcdef123".to_owned()),
        ("X-Key".to_owned(), "k1".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        (
            "Date".to_owned(),
            "Tue, 17 Nov 2026 10:00:00 GMT".to_owned(),
        ),
    ];
    let exchange = Exchange {
        headers: &headers,
        ..Exchange::default()
    };
    let redactor = Redactor::new();
    let normalized = Normalizer::new(&s, &redactor).unwrap().normalize(&exchange);
    let stored: Vec<(&str, &str)> = normalized
        .headers
        .iter()
        .flatten()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        stored,
        [
            ("authorization", "«redacted:Authorization»"),
            ("content-type", "application/json"),
            ("x-key", REDACTED_HEADER),
        ]
    );
    assert_eq!((normalized.status, normalized.body), (None, None));
}

#[test]
fn repeated_headers_are_joined() {
    let s = spec("compare: [headers]\nheaders: {include: [vary]}\n");
    let headers = vec![
        ("Vary".to_owned(), "accept".to_owned()),
        ("vary".to_owned(), "origin".to_owned()),
    ];
    let exchange = Exchange {
        headers: &headers,
        ..Exchange::default()
    };
    let redactor = Redactor::new();
    let normalized = Normalizer::new(&s, &redactor).unwrap().normalize(&exchange);
    assert_eq!(normalized.headers.unwrap()["vary"], "accept, origin");
}

#[test]
fn ignored_paths_become_placeholders() {
    let s = spec("ignore: [\"$.id\", \"$.items[*].created_at\", \"$.nothing\"]\n");
    let value = json_of(
        &s,
        r#"{"id":9,"items":[{"created_at":1,"n":"a"},{"created_at":2,"n":"b"}]}"#,
    );
    assert_eq!(
        value,
        json!({"id": IGNORED, "items": [{"created_at": IGNORED, "n": "a"}, {"created_at": IGNORED, "n": "b"}]})
    );
}

#[test]
fn default_presets_mask_timestamps_and_uuids_only() {
    let body = r#"{"updated":"2026-11-17T10:00:00Z","id":"3f2504e0-4f89-11d3-9a0c-0305e82c3301","day":"2026-11-17"}"#;
    let value = json_of(&SnapshotAssertion::default(), body);
    assert_eq!(value["updated"], "«timestamp»");
    assert_eq!(value["id"], "«uuid»");
    assert_eq!(value["day"], "2026-11-17");
    let verbatim = json_of(&spec("presets: []\n"), body);
    assert_eq!(verbatim["updated"], "2026-11-17T10:00:00Z");
}

#[test]
fn epoch_ms_needs_a_time_like_field() {
    let s = spec("presets: [epoch_ms]\n");
    let value = json_of(
        &s,
        r#"{"created_at":1763373600000,"time":"1763373600000","count":1763373600000,"ts_at":17}"#,
    );
    assert_eq!(value["created_at"], EPOCH_MS);
    assert_eq!(value["time"], EPOCH_MS);
    assert_eq!(value["count"], 1_763_373_600_000_u64);
    assert_eq!(value["ts_at"], 17);
}

#[test]
fn replacements_run_in_order_on_strings() {
    let s = SnapshotAssertion {
        replace: Some(vec![
            Replacement {
                pattern: "ORD-\\d+".to_owned().into(),
                with: "ORD-«n»".to_owned(),
            },
            Replacement {
                pattern: "«n»".to_owned().into(),
                with: "«order»".to_owned(),
            },
        ]),
        ..SnapshotAssertion::default()
    };
    let a = json_of(&s, r#"{"order":"ORD-123","n":5}"#);
    let b = json_of(&s, r#"{"order":"ORD-987","n":5}"#);
    assert_eq!(a, b);
    assert_eq!(a["order"], "ORD-«order»");
    let NormalizedBody::Text(text) = body_of(&s, &Redactor::new(), "see ORD-42\n") else {
        panic!("text body expected");
    };
    assert_eq!(text, "see ORD-«order»\n");
}

#[test]
fn sort_rules_order_arrays() {
    let s = SnapshotAssertion {
        sort: Some(vec![
            SortRule {
                path: "$.users".to_owned().into(),
                by: Some("id".to_owned()),
            },
            SortRule {
                path: "$.tags".to_owned().into(),
                by: None,
            },
        ]),
        ..SnapshotAssertion::default()
    };
    let a = json_of(
        &s,
        r#"{"users":[{"id":10},{"id":9},{"id":2}],"tags":["b","a"]}"#,
    );
    let b = json_of(
        &s,
        r#"{"tags":["a","b"],"users":[{"id":2},{"id":10},{"id":9}]}"#,
    );
    assert_eq!(a, b);
    assert_eq!(a["users"], json!([{"id": 2}, {"id": 9}, {"id": 10}]));
    let unsorted = SnapshotAssertion::default();
    assert_ne!(
        json_of(&unsorted, r#"{"users":[1,2]}"#),
        json_of(&unsorted, r#"{"users":[2,1]}"#)
    );
}

#[test]
fn canonical_form_sorts_keys() {
    let value = json_of(
        &SnapshotAssertion::default(),
        r#"{"b":{"y":1,"x":2},"a":0}"#,
    );
    assert_eq!(value.to_string(), r#"{"a":0,"b":{"x":2,"y":1}}"#);
}

#[test]
fn bodies_are_typed_by_content() {
    let redactor = Redactor::new();
    let s = SnapshotAssertion::default();
    let normalizer = Normalizer::new(&s, &redactor).unwrap();
    let body = |content_type: Option<&str>, bytes: &[u8]| {
        let headers: Vec<(String, String)> = content_type
            .map(|c| vec![("content-type".to_owned(), c.to_owned())])
            .unwrap_or_default();
        let exchange = Exchange {
            headers: &headers,
            body: bytes,
            ..Exchange::default()
        };
        normalizer.normalize(&exchange).body.unwrap()
    };
    assert!(matches!(body(None, b"[1]"), NormalizedBody::Json(_)));
    assert!(matches!(body(None, b"hello"), NormalizedBody::Text(_)));
    assert!(matches!(
        body(Some("text/plain"), b"{\"a\":1}"),
        NormalizedBody::Text(_)
    ));
    assert!(matches!(
        body(Some("application/json"), b"{bad"),
        NormalizedBody::Text(_)
    ));
    let png = body(Some("image/png"), b"\x89PNG");
    assert_eq!(
        png,
        NormalizedBody::Binary {
            size: 4,
            sha256: sha256_hex(b"\x89PNG")
        }
    );
    assert!(matches!(
        body(None, &[0xff, 0xfe]),
        NormalizedBody::Binary { size: 2, .. }
    ));
}

#[test]
fn bodies_over_one_mebibyte_are_hashed_with_a_warning() {
    let redactor = Redactor::new();
    let s = SnapshotAssertion::default();
    let big = format!("[\"{}\"]", "x".repeat(MAX_STORED_BODY));
    let headers = json_headers();
    let exchange = Exchange {
        headers: &headers,
        body: big.as_bytes(),
        ..Exchange::default()
    };
    let normalized = Normalizer::new(&s, &redactor).unwrap().normalize(&exchange);
    assert!(matches!(
        normalized.body,
        Some(NormalizedBody::Binary { .. })
    ));
    assert_eq!(normalized.warnings.len(), 1);
    assert!(normalized.warnings[0].contains("over 1 MiB"));
}

#[test]
fn events_are_normalized_one_by_one() {
    let s = spec("ignore: ['$.id']\n");
    let events = [json!({"id": 1, "n": "a"}), json!({"id": 2, "n": "b"})];
    let exchange = Exchange {
        events: Some(&events),
        ..Exchange::default()
    };
    let redactor = Redactor::new();
    let normalized = Normalizer::new(&s, &redactor).unwrap().normalize(&exchange);
    assert_eq!(
        normalized.body,
        Some(NormalizedBody::Events(vec![
            json!({"id": IGNORED, "n": "a"}),
            json!({"id": IGNORED, "n": "b"})
        ]))
    );
}

fn arbitrary_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-zA-Z0-9 :TZ.-]{0,30}".prop_map(Value::String),
        Just(json!("2026-11-17T10:00:00Z")),
        Just(json!("3f2504e0-4f89-11d3-9a0c-0305e82c3301")),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map("[a-z_]{1,8}", inner, 0..6)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #[test]
    fn normalizing_twice_gives_identical_output(value in arbitrary_json()) {
        let s = spec(
            "ignore: ['$.id']\npresets: [timestamps, dates, uuids, etags, epoch_ms, jwt]\nreplace: [{pattern: 'ab+', with: 'X'}]\nsort: [{path: '$.list'}]\n",
        );
        let redactor = Redactor::new().with(&Secret::new("s", "secret-value".to_owned()));
        let normalizer = Normalizer::new(&s, &redactor).unwrap();
        let once = normalizer.json(value);
        let twice = normalizer.json(once.clone());
        prop_assert_eq!(once.to_string(), twice.to_string());
    }
}
