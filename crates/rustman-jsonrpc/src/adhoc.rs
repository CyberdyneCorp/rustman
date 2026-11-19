//! Ad-hoc `kind: jsonrpc` requests: an endpoint and call fields typed on a
//! command line or sent by an agent, loaded exactly like a request file.

use std::path::PathBuf;

use rustman_core::LoadError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{JsonrpcRequest, REQUEST_FILE_SUFFIX, Request};
use serde_json::{Map, Value, json};

/// The name ad-hoc calls are recorded under.
pub const AD_HOC: &str = "ad-hoc";

/// An ad-hoc request to `endpoint` with the request-file `fields`
/// (`method`, `params`, `headers`, ...). JSON is valid YAML, so every value
/// keeps its type and every load-time check applies.
pub fn ad_hoc_entry(endpoint: &str, fields: Map<String, Value>) -> Result<RequestEntry, LoadError> {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(1));
    doc.insert("kind".into(), json!("jsonrpc"));
    doc.insert("name".into(), json!(AD_HOC));
    doc.insert("endpoint".into(), json!(endpoint));
    doc.extend(fields);
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let text = Value::Object(doc).to_string();
    let request = JsonrpcRequest::parse(&path, &text)?;
    Ok(RequestEntry {
        path,
        request: Request::Jsonrpc(Box::new(request)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_hoc_requests_load_like_files() {
        let mut fields = Map::new();
        fields.insert("method".into(), json!("getblockcount"));
        let entry = ad_hoc_entry("http://127.0.0.1:8332", fields).unwrap();
        let Request::Jsonrpc(request) = &entry.request else {
            panic!("not jsonrpc");
        };
        assert_eq!(request.method.as_deref(), Some("getblockcount"));
        let mut bad = Map::new();
        bad.insert("params".into(), json!(5));
        bad.insert("method".into(), json!("m"));
        let err = ad_hoc_entry("http://x", bad).unwrap_err();
        assert!(
            err.message.contains("params must be a list or a map"),
            "{err}"
        );
    }
}
