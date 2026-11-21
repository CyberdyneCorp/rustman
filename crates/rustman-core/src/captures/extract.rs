//! Reading one capture's value out of a response.

use serde_json::Value;
use serde_json_path::JsonPath;

use crate::assertions::{ResponseView, regex};
use crate::model::{Capture, CaptureSource, OutputRef};

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
        CaptureSource::EvmOutput | CaptureSource::EvmEvent | CaptureSource::EvmReceipt => {
            evm(capture, response)
        }
    }
}

/// An EVM capture reads the document `rustman-evm` puts in the body:
/// `{"outputs": [{name, value}], "events": [{signature, fields}],
/// "receipt": {...}}`, its values already decimal strings and checksummed
/// addresses.
fn evm(capture: &Capture, response: &ResponseView<'_>) -> Result<String, Miss> {
    let document = response
        .json()
        .ok()
        .filter(|d| d.get("outputs").is_some())
        .ok_or_else(|| Miss::Error(format!("{}: not an EVM result", capture.from.as_str())))?;
    let field = capture.field.as_deref().unwrap_or_default();
    let found = match capture.from {
        CaptureSource::EvmOutput => evm_output(&document, capture.output.as_ref()),
        CaptureSource::EvmEvent => {
            let event = capture.event.as_deref().unwrap_or_default();
            evm_event(&document, event, field)
        }
        _ => document.get("receipt").and_then(|r| r.get(field)).cloned(),
    };
    match found {
        Some(Value::String(text)) => Ok(text),
        Some(Value::Null) | None => Err(Miss::NoMatch(evm_missing(capture, field))),
        Some(other) => Ok(other.to_string()),
    }
}

fn evm_output(document: &Value, output: Option<&OutputRef>) -> Option<Value> {
    let outputs = document.get("outputs")?.as_array()?;
    let found = match output? {
        OutputRef::Index(index) => outputs.get(*index),
        OutputRef::Name(name) => outputs
            .iter()
            .find(|o| o.get("name").and_then(Value::as_str) == Some(name.as_str())),
    };
    found?.get("value").cloned()
}

fn evm_event(document: &Value, event: &str, field: &str) -> Option<Value> {
    let wanted = event.replace(' ', "");
    document
        .get("events")?
        .as_array()?
        .iter()
        .filter(|e| e.get("removed").and_then(Value::as_bool) != Some(true))
        .find(|e| e.get("signature").and_then(Value::as_str) == Some(wanted.as_str()))?
        .get("fields")?
        .get(field)
        .cloned()
}

fn evm_missing(capture: &Capture, field: &str) -> String {
    match capture.from {
        CaptureSource::EvmOutput => {
            let output = capture.output.as_ref().map(ToString::to_string);
            format!("no output {}", output.unwrap_or_default())
        }
        CaptureSource::EvmEvent => format!(
            "no {} event with field {field}",
            capture.event.as_deref().unwrap_or_default()
        ),
        _ => format!("no receipt field {field}"),
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
            output: None,
            event: None,
            field: None,
            required: None,
            persist: None,
        }
    }

    #[test]
    fn evm_captures_read_the_evm_document() {
        let doc = r#"{"outputs": [{"name": "", "value": "0xAbC"}, {"name": "balance", "value": "42"}],
            "events": [{"signature": "Transfer(address,address,uint256)", "removed": true, "fields": {"to": "0xOld"}},
                       {"signature": "Transfer(address,address,uint256)", "fields": {"to": "0xNew", "value": "7"}}],
            "receipt": {"blockNumber": "12", "status": true}}"#;
        let r = response(doc);
        let out = |o: OutputRef| Capture {
            output: Some(o),
            ..capture(CaptureSource::EvmOutput)
        };
        assert_eq!(extract(&out(OutputRef::Index(0)), &r), Ok("0xAbC".into()));
        assert_eq!(
            extract(&out(OutputRef::Name("balance".into())), &r),
            Ok("42".into())
        );
        assert_eq!(
            extract(&out(OutputRef::Index(5)), &r),
            Err(Miss::NoMatch("no output 5".into()))
        );
        let event = Capture {
            event: Some("Transfer(address, address, uint256)".into()),
            field: Some("to".into()),
            ..capture(CaptureSource::EvmEvent)
        };
        assert_eq!(
            extract(&event, &r),
            Ok("0xNew".into()),
            "reorged logs are skipped"
        );
        let receipt = |f: &str| Capture {
            field: Some(f.into()),
            ..capture(CaptureSource::EvmReceipt)
        };
        assert_eq!(extract(&receipt("blockNumber"), &r), Ok("12".into()));
        assert_eq!(extract(&receipt("status"), &r), Ok("true".into()));
        assert_eq!(
            extract(&receipt("gasUsed"), &r),
            Err(Miss::NoMatch("no receipt field gasUsed".into()))
        );
        assert_eq!(
            extract(&receipt("gasUsed"), &response("{}")),
            Err(Miss::Error("evm_receipt: not an EVM result".into()))
        );
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
