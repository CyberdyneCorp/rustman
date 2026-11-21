use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::capture::Tunnel;

const SCHEMA: &str = include_str!("../../tests/fixtures/har-1.2.schema.json");

/// The schema errors of `doc`, empty when it is valid HAR 1.2.
fn schema_errors(doc: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    validator.iter_errors(doc).map(|e| e.to_string()).collect()
}

fn capture(id: u64, recorded: Recorded) -> Capture {
    Capture {
        id,
        started: UNIX_EPOCH + Duration::from_millis(1_700_000_000_123),
        recorded,
    }
}

fn body(bytes: &[u8]) -> BodyCapture {
    BodyCapture {
        bytes: bytes.to_vec(),
        total: bytes.len() as u64,
        truncated: false,
    }
}

fn exchange() -> Exchange {
    Exchange {
        method: "POST".into(),
        url: "https://api.test/v1/login?api_key=qk-112233445566&page=2".into(),
        version: "HTTP/1.1".into(),
        request_headers: vec![
            ("authorization".into(), "Bearer tok-abcdef123456".into()),
            ("cookie".into(), "sid=c00kie-value-456".into()),
            ("x-api-key".into(), "xk-998877665544".into()),
            ("content-type".into(), "application/json".into()),
        ],
        request_body: body(br#"{"user":"ann","password":"hunter2-pass"}"#),
        status: Some(201),
        response_headers: vec![
            ("content-type".into(), "application/json".into()),
            ("set-cookie".into(), "sid=newsecret99; Path=/".into()),
        ],
        response_body: body(br#"{"echo":"Bearer tok-abcdef123456","token":"rt-5566778899"}"#),
        duration: Duration::from_millis(12),
        intercepted: true,
        error: None,
    }
}

fn three() -> Vec<Capture> {
    let mut truncated = exchange();
    truncated.response_body = BodyCapture {
        bytes: vec![0xff; 4],
        total: 10,
        truncated: true,
    };
    let tunnel = Tunnel {
        host: "www.google.com".into(),
        port: 443,
        bytes_up: 10,
        bytes_down: 20,
        duration: Duration::from_millis(5),
        error: None,
    };
    vec![
        capture(1, Recorded::Exchange(Box::new(exchange()))),
        capture(2, Recorded::Tunnel(tunnel)),
        capture(3, Recorded::Exchange(Box::new(truncated))),
    ]
}

const SECRETS: [&str; 7] = [
    "tok-abcdef123456",
    "c00kie-value-456",
    "xk-998877665544",
    "qk-112233445566",
    "hunter2-pass",
    "rt-5566778899",
    "newsecret99",
];

#[test]
fn the_export_validates_against_the_har_schema() {
    let doc = har(&three(), false);
    assert_eq!(schema_errors(&doc), Vec::<String>::new());
    assert_eq!(doc["log"]["version"], "1.2");
    assert_eq!(doc["log"]["entries"].as_array().unwrap().len(), 3);
    let shown = har(&three(), true);
    assert_eq!(schema_errors(&shown), Vec::<String>::new());
}

#[test]
fn the_schema_rejects_an_entry_without_a_response() {
    let mut doc = har(&three(), false);
    doc["log"]["entries"][0]
        .as_object_mut()
        .unwrap()
        .remove("response");
    assert!(!schema_errors(&doc).is_empty());
}

#[test]
fn secrets_are_redacted_everywhere() {
    let doc = har(&three(), false);
    let text = doc.to_string();
    for secret in SECRETS {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    let request = &doc["log"]["entries"][0]["request"];
    let auth = request["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "authorization")
        .unwrap();
    assert_eq!(auth["value"], "«redacted:Authorization»");
    assert!(
        request["postData"]["text"]
            .as_str()
            .unwrap()
            .contains("\"user\":\"ann\"")
    );
}

#[test]
fn show_secrets_keeps_the_values() {
    let text = har(&three(), true).to_string();
    for secret in SECRETS {
        assert!(text.contains(secret), "{secret} missing");
    }
}

#[test]
fn tunnels_and_truncated_bodies_are_commented() {
    let doc = har(&three(), false);
    let entries = doc["log"]["entries"].as_array().unwrap();
    assert_eq!(entries[1]["request"]["method"], "CONNECT");
    assert_eq!(entries[1]["request"]["url"], "https://www.google.com:443/");
    assert_eq!(
        entries[1]["comment"],
        "tunnel: not decrypted; 10 B up, 20 B down"
    );
    let comment = entries[2]["comment"].as_str().unwrap();
    assert!(
        comment.contains("truncated: response body stored 4 of 10 bytes"),
        "{comment}"
    );
    assert_eq!(entries[2]["response"]["content"]["encoding"], "base64");
    assert_eq!(entries[2]["response"]["content"]["text"], "/////w==");
    assert_eq!(entries[0]["startedDateTime"], "2023-11-14T22:13:20.123Z");
    assert_eq!(entries[0]["response"]["statusText"], "Created");
}

#[test]
fn a_rejected_handshake_carries_the_hint() {
    let rejected = Recorded::TlsRejected {
        host: "api.example.com".into(),
        port: 443,
    };
    let doc = har(&[capture(1, rejected)], false);
    assert_eq!(schema_errors(&doc), Vec::<String>::new());
    assert_eq!(
        doc["log"]["entries"][0]["comment"],
        format!("TLS REJECTED: {TLS_REJECTED_HINT}")
    );
}

#[test]
fn write_har_creates_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out/session.har");
    write_har(&path, &three(), false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(schema_errors(&doc), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn write_har_is_readable_by_the_user_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.har");
    std::fs::write(&path, "old").unwrap();
    write_har(&path, &three(), true).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
