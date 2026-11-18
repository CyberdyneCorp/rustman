//! Events as JSON objects for `rustman webhook listen --jsonl` (spec: CLI
//! streaming output): one object per event, with a `type` field. Every text
//! taken from a request goes through the [`Redactor`] first.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustman_core::Redactor;
use serde_json::{Value, json};

use crate::capture::Capture;
use crate::event::Event;

/// `event` as one JSON object, redacted with `redactor`.
pub fn event_json(event: &Event, redactor: &Redactor) -> Value {
    match event {
        Event::Captured(capture) => capture_json(capture, redactor),
        Event::Rejected(rejection) => json!({
            "type": "rejection",
            "remote": rejection.remote.to_string(),
            "status": rejection.status,
            "reason": redactor.text(&rejection.reason),
        }),
        Event::MockMatched {
            id,
            method,
            pattern,
            status,
            delay_ms,
        } => json!({
            "type": "mock",
            "id": id,
            "method": redactor.text(method),
            "pattern": redactor.text(pattern),
            "status": status,
            "delay_ms": delay_ms,
        }),
        Event::MocksReloaded { count } => json!({ "type": "mocks_reloaded", "count": count }),
        Event::MocksInvalid { message } => json!({
            "type": "mocks_invalid",
            "message": redactor.text(message),
        }),
        Event::Cleared => json!({ "type": "cleared" }),
    }
}

/// A capture: `body` when it is UTF-8 text, else `body_base64`.
fn capture_json(capture: &Capture, redactor: &Redactor) -> Value {
    let headers: Vec<Value> = capture
        .headers
        .iter()
        .map(|h| {
            json!({
                "name": redactor.text(&h.name),
                "value": redactor.header_value(&h.name, &h.value_text()),
            })
        })
        .collect();
    let mut object = json!({
        "type": "capture",
        "id": capture.id,
        "ts": capture.timestamp(),
        "remote": capture.remote.to_string(),
        "version": capture.version_text(),
        "method": redactor.text(&capture.method),
        "path": redactor.text(&capture.path),
        "query": capture.query.as_deref().map(|q| redactor.text(q).into_owned()),
        "status": capture.status,
        "mock": capture.mock.as_deref().map(|m| redactor.text(m).into_owned()),
        "signature": capture.signature.map(|s| s.as_str()),
        "headers": headers,
        "body_size": capture.body_size(),
    });
    let (key, body) = match std::str::from_utf8(&capture.body) {
        Ok(text) => ("body", redactor.text(text).into_owned()),
        Err(_) => (
            "body_base64",
            STANDARD.encode(redactor.bytes(&capture.body)),
        ),
    };
    object[key] = Value::String(body);
    object
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use bytes::Bytes;
    use rustman_core::Secret;

    use super::*;
    use crate::capture::HeaderEntry;
    use crate::capture::tests::capture;
    use crate::event::Rejection;
    use crate::signature::Signature;

    fn redactor() -> Redactor {
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("gh_hook", "canary-7f3a".to_owned()));
        redactor
    }

    #[test]
    fn a_text_capture_has_every_field_and_body() {
        let mut c = capture("/x", b"{\"a\":1}");
        c.id = 4;
        c.query = Some("debug=1".into());
        c.signature = Some(Signature::Valid);
        c.headers.push(HeaderEntry::new("X-A", "1"));
        c.headers.push(HeaderEntry::new("X-A", "2"));
        let value = event_json(&Event::Captured(Arc::new(c)), &redactor());
        assert_eq!(
            value,
            json!({
                "type": "capture",
                "id": 4,
                "ts": "1970-01-01T00:00:00Z",
                "remote": "127.0.0.1:5000",
                "version": "HTTP/1.1",
                "method": "POST",
                "path": "/x",
                "query": "debug=1",
                "status": 200,
                "mock": null,
                "signature": "valid",
                "headers": [
                    {"name": "Content-Type", "value": "application/json"},
                    {"name": "X-A", "value": "1"},
                    {"name": "X-A", "value": "2"},
                ],
                "body_size": 7,
                "body": "{\"a\":1}",
            })
        );
    }

    #[test]
    fn a_binary_body_is_base64_and_secrets_are_redacted() {
        let mut c = capture("/canary-7f3a", b"");
        c.body = Bytes::from_static(&[0xff, 0xfe, 0x00]);
        c.query = Some("k=canary-7f3a".into());
        c.headers.push(HeaderEntry::new("X-Echo", "canary-7f3a"));
        c.headers
            .push(HeaderEntry::new("Authorization", "Bearer abc"));
        let value = event_json(&Event::Captured(Arc::new(c)), &redactor());
        assert_eq!(value["body_base64"], "//4A");
        assert!(value.get("body").is_none());
        let text = value.to_string();
        assert!(!text.contains("canary-7f3a"), "{text}");
        assert!(!text.contains("Bearer abc"), "{text}");
        assert!(text.contains("«redacted:gh_hook»"), "{text}");
    }

    #[test]
    fn other_events_name_their_type() {
        let remote: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let types: Vec<Value> = [
            Event::Rejected(Rejection {
                remote,
                status: 431,
                reason: "too many header fields".into(),
            }),
            Event::MockMatched {
                id: 1,
                method: "POST".into(),
                pattern: "/h".into(),
                status: 500,
                delay_ms: 0,
            },
            Event::MocksReloaded { count: 2 },
            Event::MocksInvalid {
                message: "bad".into(),
            },
            Event::Cleared,
        ]
        .iter()
        .map(|e| event_json(e, &redactor())["type"].clone())
        .collect();
        assert_eq!(
            types,
            [
                "rejection",
                "mock",
                "mocks_reloaded",
                "mocks_invalid",
                "cleared"
            ]
        );
        let rejection = event_json(
            &Event::Rejected(Rejection {
                remote,
                status: 431,
                reason: "too many header fields".into(),
            }),
            &redactor(),
        );
        assert_eq!(rejection["status"], 431);
        assert_eq!(rejection["reason"], "too many header fields");
    }

    #[test]
    fn header_names_methods_and_mock_patterns_are_redacted_too() {
        let mut leaky = capture("/h", b"");
        leaky.method = "canary-7f3a".into();
        leaky.mock = Some("/canary-7f3a".into());
        leaky.headers.push(HeaderEntry::new("X-canary-7f3a", "v"));
        let mock = Event::MockMatched {
            id: 1,
            method: "canary-7f3a".into(),
            pattern: "/canary-7f3a/*".into(),
            status: 200,
            delay_ms: 0,
        };
        for event in [Event::Captured(Arc::new(leaky)), mock] {
            let line = event_json(&event, &redactor()).to_string();
            assert!(!line.contains("canary-7f3a"), "{line}");
            assert!(line.contains("redacted:gh_hook"), "{line}");
        }
    }
}
