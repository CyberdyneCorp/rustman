//! Ad-hoc `kind: kafka`, `kind: mqtt` and `kind: amqp` requests: fields
//! typed on a command line, loaded exactly like a request file so every
//! load-time check applies.

use std::path::PathBuf;

use rustman_core::LoadError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{MessagingRequest, REQUEST_FILE_SUFFIX, Request};
use serde_json::{Map, Value, json};

/// The name ad-hoc operations are recorded under.
pub const AD_HOC: &str = "ad-hoc";

/// An ad-hoc request of `kind` with the request-file `fields`
/// (`operation`, `brokers`, `topic`, `messages`...).
pub fn ad_hoc_entry(kind: &str, fields: Map<String, Value>) -> Result<RequestEntry, LoadError> {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(1));
    doc.insert("kind".into(), json!(kind));
    doc.insert("name".into(), json!(AD_HOC));
    doc.extend(fields);
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let text = Value::Object(doc).to_string();
    let request = MessagingRequest::parse(&path, &text)?;
    Ok(RequestEntry {
        path,
        request: Request::Messaging(Box::new(request)),
    })
}

/// `"Name: value"` lines as message `headers`.
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
    fn ad_hoc_requests_load_like_files() {
        let fields = json!({
            "operation": "produce",
            "brokers": "localhost:9092",
            "topic": "orders",
            "messages": [{"value": "a", "headers": header_fields(&["X-Id: 1".into()]).unwrap()}],
        });
        let Value::Object(fields) = fields else {
            unreachable!()
        };
        let entry = ad_hoc_entry("kafka", fields).unwrap();
        assert_eq!(entry.request.kind(), "kafka");
        assert_eq!(entry.request.name(), AD_HOC);
        let missing = ad_hoc_entry("mqtt", Map::new()).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("messaging: operation must be produce or consume"),
            "{missing}"
        );
        assert!(header_fields(&["nocolon".into()]).is_err());
    }
}
