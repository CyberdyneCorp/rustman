//! Ad-hoc `kind: grpc` requests: a target and call fields typed on a
//! command line or sent by an agent, loaded exactly like a request file.

use std::path::PathBuf;

use rustman_core::LoadError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{GrpcRequest, REQUEST_FILE_SUFFIX, Request};
use serde_json::{Map, Value, json};

/// The name ad-hoc calls are recorded under.
pub const AD_HOC: &str = "ad-hoc";

/// The method of an ad-hoc request that only names a target (to list or
/// describe its services): the standard health check.
pub const TARGET_ONLY_METHOD: &str = "grpc.health.v1.Health/Check";

/// An ad-hoc request to `target` with the request-file `fields`
/// (`method`, `message` or `messages`, `metadata`, ...). JSON is valid
/// YAML, so every value keeps its type and every load-time check applies.
pub fn ad_hoc_entry(target: &str, fields: Map<String, Value>) -> Result<RequestEntry, LoadError> {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(1));
    doc.insert("kind".into(), json!("grpc"));
    doc.insert("name".into(), json!(AD_HOC));
    doc.insert("target".into(), json!(target));
    doc.extend(fields);
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let text = Value::Object(doc).to_string();
    let request = GrpcRequest::parse(&path, &text)?;
    Ok(RequestEntry {
        path,
        request: Request::Grpc(Box::new(request)),
    })
}

/// An ad-hoc request that only names `target`.
pub fn target_entry(target: &str) -> Result<RequestEntry, LoadError> {
    let mut fields = Map::new();
    fields.insert("method".into(), json!(TARGET_ONLY_METHOD));
    fields.insert("message".into(), json!({}));
    ad_hoc_entry(target, fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_hoc_requests_load_like_files() {
        let mut fields = Map::new();
        fields.insert("method".into(), json!("shop.v1.Catalog/UploadProducts"));
        fields.insert("messages".into(), json!([{"id": "a"}, {"id": "b"}]));
        let entry = ad_hoc_entry("grpc://127.0.0.1:50051", fields).unwrap();
        let Request::Grpc(request) = &entry.request else {
            panic!("not grpc");
        };
        assert_eq!(request.message_list().len(), 2);
        assert_eq!(entry.id(), AD_HOC);
        let entry = target_entry("{{grpc_host}}").unwrap();
        assert_eq!(entry.request.kind(), "grpc");
        let err = target_entry("http://h:1").unwrap_err();
        assert!(err.message.contains("target must be"), "{err}");
    }
}
