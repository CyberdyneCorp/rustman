//! Ad-hoc `evm-call` requests, built from command-line flags or agent tool
//! arguments, so they load, resolve and run exactly like request files.

use std::path::PathBuf;

use rustman_core::LoadError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{EvmRequest, REQUEST_FILE_SUFFIX, Request};
use serde_json::{Map, Value, json};

/// The name and file stem of ad-hoc requests.
pub const AD_HOC: &str = "ad-hoc";

/// A `kind: evm-call` request to `rpc` with the request-file `fields`
/// (`to`, `abi`, `function`, `args`, `block`, ...). ABI paths resolve from
/// the workspace root.
pub fn call_entry(rpc: &str, fields: Map<String, Value>) -> Result<RequestEntry, LoadError> {
    let mut doc = Map::new();
    doc.insert("version".into(), json!(1));
    doc.insert("kind".into(), json!(rustman_core::model::EVM_CALL_KIND));
    doc.insert("name".into(), json!(AD_HOC));
    doc.insert("rpc".into(), json!(rpc));
    doc.extend(fields);
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let request = EvmRequest::parse(&path, &Value::Object(doc).to_string())?;
    Ok(RequestEntry {
        path,
        request: Request::Evm(Box::new(request)),
    })
}

/// A command-line argument as an ABI argument: JSON lists, objects and
/// booleans as JSON, anything else (numbers included, so 256-bit values
/// keep their precision) as text.
pub fn arg_value(text: &str) -> Value {
    let trimmed = text.trim();
    let structured = trimmed.starts_with('[') || trimmed.starts_with('{');
    if (structured || trimmed == "true" || trimmed == "false")
        && let Ok(value) = serde_json::from_str(trimmed)
    {
        return value;
    }
    Value::String(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_hoc_calls_load_like_files() {
        let mut fields = Map::new();
        fields.insert(
            "to".into(),
            json!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        );
        fields.insert("abi".into(), json!("erc20"));
        fields.insert("function".into(), json!("balanceOf"));
        fields.insert("args".into(), json!(["vitalik.eth"]));
        let entry = call_entry("{{rpc}}", fields).unwrap();
        let Request::Evm(evm) = &entry.request else {
            panic!("not evm");
        };
        assert_eq!(evm.common().rpc.get(), "{{rpc}}");
        assert_eq!(entry.stem(), AD_HOC);
        assert!(call_entry("x", Map::new()).is_err(), "to is required");
    }

    #[test]
    fn arguments_keep_their_precision() {
        assert_eq!(
            arg_value(
                "115792089237316195423570985008687907853269984665640564039457584007913129639935"
            ),
            json!("115792089237316195423570985008687907853269984665640564039457584007913129639935")
        );
        assert_eq!(arg_value("[1, 2]"), json!([1, 2]));
        assert_eq!(arg_value("true"), json!(true));
        assert_eq!(arg_value("1.5 ether"), json!("1.5 ether"));
        assert_eq!(arg_value("[oops"), json!("[oops"));
    }
}
