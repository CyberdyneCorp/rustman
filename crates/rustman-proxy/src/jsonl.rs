//! Captures as JSON lines for `rustman record --json` (spec: Live capture
//! list, Redaction in display and export): one object per capture, every
//! text redacted with the capture's [`Capture::redactor`], sensitive
//! headers replaced whole. Bodies are left out; their sizes are given.

use std::time::{SystemTime, UNIX_EPOCH};

use rustman_core::Redactor;
use serde_json::{Value, json};

use crate::capture::{Capture, Exchange, Header, Recorded, TLS_REJECTED_HINT};

/// `capture` as one redacted JSON object.
pub fn capture_json(capture: &Capture) -> Value {
    let redactor = capture.redactor();
    let mut object = match &capture.recorded {
        Recorded::Exchange(exchange) => exchange_json(exchange, &redactor),
        Recorded::Tunnel(t) => json!({
            "type": "tunnel",
            "host": t.host,
            "port": t.port,
            "bytes_up": t.bytes_up,
            "bytes_down": t.bytes_down,
            "duration_ms": millis(t.duration),
            "error": t.error.as_deref().map(|e| redactor.text(e).into_owned()),
        }),
        Recorded::TlsRejected { host, port } => json!({
            "type": "tls_rejected",
            "host": host,
            "port": port,
            "hint": TLS_REJECTED_HINT,
        }),
    };
    object["id"] = json!(capture.id);
    object["ts_ms"] = json!(epoch_millis(capture.started));
    object
}

/// One line: the compact JSON of [`capture_json`].
pub fn capture_line(capture: &Capture) -> String {
    capture_json(capture).to_string()
}

fn exchange_json(e: &Exchange, redactor: &Redactor) -> Value {
    json!({
        "type": "exchange",
        "method": e.method,
        "url": redactor.text(&e.url),
        "version": e.version,
        "status": e.status,
        "duration_ms": millis(e.duration),
        "intercepted": e.intercepted,
        "request_headers": headers_json(&e.request_headers, redactor),
        "response_headers": headers_json(&e.response_headers, redactor),
        "request_size": e.request_body.total,
        "response_size": e.response_body.total,
        "truncated": e.request_body.truncated || e.response_body.truncated,
        "error": e.error.as_deref().map(|m| redactor.text(m).into_owned()),
    })
}

fn headers_json(headers: &[Header], redactor: &Redactor) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name,
                "value": redactor.header_value(name, value),
            })
        })
        .collect()
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn epoch_millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, millis)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::capture::{BodyCapture, Tunnel};

    fn capture(recorded: Recorded) -> Capture {
        Capture {
            id: 7,
            started: UNIX_EPOCH + Duration::from_millis(1_700_000_000_123),
            recorded,
        }
    }

    fn exchange() -> Exchange {
        Exchange {
            method: "GET".into(),
            url: "http://api.test/v1/me?echo=tok-abcdef123456".into(),
            version: "HTTP/1.1".into(),
            request_headers: vec![
                ("authorization".into(), "Bearer tok-abcdef123456".into()),
                ("cookie".into(), "sid=abc123secret".into()),
                ("accept".into(), "application/json".into()),
            ],
            request_body: BodyCapture::default(),
            status: Some(200),
            response_headers: vec![
                ("content-type".into(), "application/json".into()),
                ("set-cookie".into(), "sid=newsecret99; Path=/".into()),
            ],
            response_body: BodyCapture {
                bytes: b"{}".to_vec(),
                total: 2,
                truncated: false,
            },
            duration: Duration::from_millis(12),
            intercepted: false,
            error: None,
        }
    }

    #[test]
    fn an_exchange_line_is_redacted() {
        let line = capture_line(&capture(Recorded::Exchange(Box::new(exchange()))));
        insta::assert_snapshot!(line);
        for secret in ["tok-abcdef123456", "abc123secret", "newsecret99"] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
    }

    #[test]
    fn secret_named_headers_query_and_body_fields_are_redacted() {
        let mut e = exchange();
        e.url =
            "http://api.test/v1/users?api_key=QWERTY987654321secret&password=hunter2hunter2&x=1"
                .into();
        e.request_headers
            .push(("x-api-key".into(), "sk_live_ABCDEF1234567890abcdef".into()));
        e.request_body = BodyCapture {
            bytes: br#"{"password":"b0dy-p4ssw0rd-99"}"#.to_vec(),
            total: 31,
            truncated: false,
        };
        let capture = capture(Recorded::Exchange(Box::new(e)));
        let line = capture_line(&capture);
        let summary = capture.redactor().text(&capture.to_string()).into_owned();
        for shown in [&line, &summary] {
            for secret in [
                "QWERTY987654321secret",
                "hunter2hunter2",
                "sk_live_ABCDEF1234567890abcdef",
            ] {
                assert!(!shown.contains(secret), "{secret} in {shown}");
            }
        }
        assert!(line.contains("x=1"), "{line}");
    }

    #[test]
    fn tunnels_and_rejections_have_their_own_types() {
        let tunnel = Recorded::Tunnel(Tunnel {
            host: "www.google.com".into(),
            port: 443,
            bytes_up: 10,
            bytes_down: 20,
            duration: Duration::from_millis(5),
            error: None,
        });
        let value = capture_json(&capture(tunnel));
        assert_eq!(value["type"], "tunnel");
        assert_eq!(value["bytes_down"], 20);
        let rejected = Recorded::TlsRejected {
            host: "api.example.com".into(),
            port: 443,
        };
        let value = capture_json(&capture(rejected));
        assert_eq!(value["type"], "tls_rejected");
        assert_eq!(value["hint"], TLS_REJECTED_HINT);
        assert_eq!(value["ts_ms"], 1_700_000_000_123u64);
    }
}
