//! Ad-hoc `kind: ws` and `kind: sse` requests: a URL and request-file
//! fields typed on a command line or sent by an agent, loaded exactly like
//! a request file so every load-time check applies.

use std::path::PathBuf;

use rustman_core::LoadError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{REQUEST_FILE_SUFFIX, Request, SseRequest, WsRequest};
use serde_json::{Map, Value, json};

/// The name ad-hoc sessions are recorded under.
pub const AD_HOC: &str = "ad-hoc";

/// The realtime kind a URL opens: `ws` for `ws://` and `wss://`, `sse`
/// for anything else (`http://`, `https://`, a `{{variable}}`).
pub fn kind_of(url: &str) -> &'static str {
    let lower = url.trim_start().to_ascii_lowercase();
    if lower.starts_with("ws://") || lower.starts_with("wss://") {
        "ws"
    } else {
        "sse"
    }
}

/// An ad-hoc request of `kind` (`ws` or `sse`) to `url` with the
/// request-file `fields` (`headers`, `steps`, `limits`...).
pub fn ad_hoc_entry(
    kind: &str,
    url: &str,
    fields: Map<String, Value>,
) -> Result<RequestEntry, LoadError> {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(1));
    doc.insert("kind".into(), json!(kind));
    doc.insert("name".into(), json!(AD_HOC));
    doc.insert("url".into(), json!(url));
    doc.extend(fields);
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let text = Value::Object(doc).to_string();
    let request = if kind == "ws" {
        Request::Ws(Box::new(WsRequest::parse(&path, &text)?))
    } else {
        Request::Sse(Box::new(SseRequest::parse(&path, &text)?))
    };
    Ok(RequestEntry { path, request })
}

/// `"Name: value"` lines as the `headers` member of a request file.
pub fn header_fields(lines: &[String]) -> Result<Value, String> {
    let parsed = lines
        .iter()
        .map(|line| {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| format!("invalid header '{line}' (expected 'Name: value')"))?;
            Ok(json!({"name": name.trim(), "value": value.trim()}))
        })
        .collect::<Result<Vec<Value>, String>>()?;
    Ok(Value::Array(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheme_picks_the_kind() {
        assert_eq!(kind_of("wss://feed.example"), "ws");
        assert_eq!(kind_of("WS://feed.example"), "ws");
        assert_eq!(kind_of("https://api.example/events"), "sse");
        assert_eq!(kind_of("{{events}}"), "sse");
    }

    #[test]
    fn ad_hoc_requests_load_like_files() {
        let mut fields = Map::new();
        fields.insert(
            "headers".into(),
            header_fields(&["X-Key: {{key}}".into()]).unwrap(),
        );
        let entry = ad_hoc_entry("ws", "ws://127.0.0.1:9/feed", fields).unwrap();
        let Request::Ws(ws) = &entry.request else {
            panic!("not ws");
        };
        assert_eq!(ws.headers.as_ref().unwrap()[0].value, "{{key}}");
        let sse = ad_hoc_entry("sse", "http://127.0.0.1:9/events", Map::new()).unwrap();
        assert_eq!(sse.request.kind(), "sse");
        let wrong = ad_hoc_entry("ws", "https://example.com/feed", Map::new());
        assert!(
            wrong
                .unwrap_err()
                .to_string()
                .contains("ws: url must use ws:// or wss://")
        );
        assert!(header_fields(&["nocolon".into()]).is_err());
    }
}
