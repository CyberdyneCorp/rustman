//! Reading one capture's value out of a response.

use serde_json::Value;
use serde_json_path::JsonPath;

use crate::assertions::{ResponseView, regex};
use crate::model::{Capture, CaptureSource};

/// Why a capture produced no value.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Miss {
    /// The response has nothing matching, e.g. `no match for $.id`.
    NoMatch(String),
    /// The capture could not be evaluated, e.g. `response body is not JSON`.
    Error(String),
}

/// The captured text: a JSON string as is, any other JSON value as compact
/// JSON, a header's first value, the status code, a regex group, a cookie or
/// the first value of gRPC metadata (headers, then trailers).
pub(super) fn extract(capture: &Capture, response: &ResponseView<'_>) -> Result<String, Miss> {
    let field = |value: &Option<String>| value.clone().unwrap_or_default();
    match capture.from {
        CaptureSource::Status => Ok(response.status.to_string()),
        CaptureSource::Json => json(&field(&capture.path), response),
        CaptureSource::Header => {
            let name = field(&capture.name);
            let value = response
                .header_values(&name)
                .first()
                .map(|v| (*v).to_owned());
            value.ok_or_else(|| Miss::NoMatch(format!("no match for header {name}")))
        }
        CaptureSource::Cookie => {
            let name = field(&capture.name);
            let value = response.cookie(&name).map(str::to_owned);
            value.ok_or_else(|| Miss::NoMatch(format!("no match for cookie {name}")))
        }
        CaptureSource::BodyRegex => body_regex(&field(&capture.pattern), capture.group, response),
        CaptureSource::Metadata => metadata(&field(&capture.name), response),
    }
}

fn metadata(name: &str, response: &ResponseView<'_>) -> Result<String, Miss> {
    if response.grpc.is_none() {
        return Err(Miss::Error("metadata: not a gRPC response".to_owned()));
    }
    let value = response
        .metadata_values(name)
        .first()
        .map(|v| (*v).to_owned());
    value.ok_or_else(|| Miss::NoMatch(format!("no match for metadata {name}")))
}

fn json(path: &str, response: &ResponseView<'_>) -> Result<String, Miss> {
    let query =
        JsonPath::parse(path).map_err(|e| Miss::Error(format!("invalid JSONPath {path}: {e}")))?;
    let document = response.json().map_err(|e| Miss::Error(e.to_owned()))?;
    let nodes = query.query(&document).all();
    let value = match nodes.as_slice() {
        [] => return Err(Miss::NoMatch(format!("no match for {path}"))),
        [one] => (*one).clone(),
        many => Value::Array(many.iter().map(|v| (*v).clone()).collect()),
    };
    Ok(match value {
        Value::String(s) => s,
        other => other.to_string(),
    })
}

/// Group `group` of the first match; by default group 1, or the whole match
/// when the pattern has no groups.
fn body_regex(
    pattern: &str,
    group: Option<u32>,
    response: &ResponseView<'_>,
) -> Result<String, Miss> {
    let re = regex(pattern).map_err(Miss::Error)?;
    let text = response.text().map_err(|e| Miss::Error(e.to_owned()))?;
    let default = usize::from(re.captures_len() > 1);
    let index = group.map_or(default, |g| g as usize);
    if index >= re.captures_len() {
        return Err(Miss::Error(format!("/{pattern}/ has no group {index}")));
    }
    let found = re
        .captures(text)
        .and_then(|c| c.get(index))
        .map(|m| m.as_str().to_owned());
    found.ok_or_else(|| Miss::NoMatch(format!("no match for /{pattern}/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn response(body: &str) -> ResponseView<'static> {
        ResponseView {
            status: 201,
            headers: vec![
                ("X-Request-Id".into(), "7".into()),
                ("Set-Cookie".into(), "sid=abc; Path=/; HttpOnly".into()),
                ("set-cookie".into(), "theme=dark".into()),
            ],
            body: Cow::Owned(body.as_bytes().to_vec()),
            ..ResponseView::default()
        }
    }

    fn capture(from: CaptureSource) -> Capture {
        Capture {
            var: "v".into(),
            from,
            path: None,
            name: None,
            pattern: None,
            group: None,
            required: None,
            persist: None,
        }
    }

    fn json_at(path: &str) -> Capture {
        Capture {
            path: Some(path.into()),
            ..capture(CaptureSource::Json)
        }
    }

    #[test]
    fn json_values_become_text() {
        let r = response(r#"{"token":"t1","id":42,"tags":["a","b"],"user":{"n":1}}"#);
        assert_eq!(extract(&json_at("$.token"), &r), Ok("t1".into()));
        assert_eq!(extract(&json_at("$.id"), &r), Ok("42".into()));
        assert_eq!(extract(&json_at("$.user"), &r), Ok(r#"{"n":1}"#.into()));
        assert_eq!(
            extract(&json_at("$.tags[*]"), &r),
            Ok(r#"["a","b"]"#.into())
        );
        assert_eq!(
            extract(&json_at("$.id2"), &r),
            Err(Miss::NoMatch("no match for $.id2".into()))
        );
        assert_eq!(
            extract(&json_at("$.id"), &response("plain")),
            Err(Miss::Error("response body is not JSON".into()))
        );
        assert!(matches!(extract(&json_at("$["), &r), Err(Miss::Error(_))));
    }

    #[test]
    fn headers_cookies_and_status() {
        let r = response("");
        let header = Capture {
            name: Some("x-request-id".into()),
            ..capture(CaptureSource::Header)
        };
        assert_eq!(extract(&header, &r), Ok("7".into()));
        let cookie = |name: &str| Capture {
            name: Some(name.into()),
            ..capture(CaptureSource::Cookie)
        };
        assert_eq!(extract(&cookie("sid"), &r), Ok("abc".into()));
        assert_eq!(extract(&cookie("theme"), &r), Ok("dark".into()));
        assert_eq!(
            extract(&cookie("nope"), &r),
            Err(Miss::NoMatch("no match for cookie nope".into()))
        );
        assert_eq!(
            extract(&capture(CaptureSource::Status), &r),
            Ok("201".into())
        );
    }

    #[test]
    fn regex_groups() {
        let r = response("created order-123456 at 10:00");
        let re = |pattern: &str, group: Option<u32>| Capture {
            pattern: Some(pattern.into()),
            group,
            ..capture(CaptureSource::BodyRegex)
        };
        assert_eq!(extract(&re(r"order-(\d+)", None), &r), Ok("123456".into()));
        assert_eq!(
            extract(&re(r"order-\d+", None), &r),
            Ok("order-123456".into())
        );
        assert_eq!(extract(&re(r"(\d+):(\d+)", Some(2)), &r), Ok("00".into()));
        assert_eq!(
            extract(&re(r"order-\d+", Some(1)), &r),
            Err(Miss::Error(r"/order-\d+/ has no group 1".into()))
        );
        assert_eq!(
            extract(&re("nothing", None), &r),
            Err(Miss::NoMatch("no match for /nothing/".into()))
        );
    }

    #[test]
    fn grpc_metadata_reads_headers_then_trailers() {
        let at = |name: &str| Capture {
            name: Some(name.into()),
            ..capture(CaptureSource::Metadata)
        };
        assert_eq!(
            extract(&at("x-request-id"), &response("")),
            Err(Miss::Error("metadata: not a gRPC response".into()))
        );
        let mut r = response("");
        r.grpc = Some(crate::assertions::GrpcView {
            code: 0,
            name: "OK".into(),
            trailers: vec![("X-Trace".into(), "t-1".into())],
        });
        assert_eq!(extract(&at("x-request-id"), &r), Ok("7".into()));
        assert_eq!(extract(&at("x-trace"), &r), Ok("t-1".into()));
        assert_eq!(
            extract(&at("nope"), &r),
            Err(Miss::NoMatch("no match for metadata nope".into()))
        );
    }
}
