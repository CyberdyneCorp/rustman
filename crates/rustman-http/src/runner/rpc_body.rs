//! JSON-RPC calls inside plain `kind: http` bodies (evm-toolkit "Send
//! methods blocked outside the signing gate"). A body that is a JSON-RPC
//! call or batch (an object with a string `method`, or an array of them)
//! is checked, before anything is sent, with the method check a front end
//! installs: `rustman_evm::install` registers the `rustman-jsonrpc` method
//! guards here, so `eth_sendTransaction` posted as an http request is
//! refused like it is from a `jsonrpc` file, with the guard's message. A
//! refusal is recorded as a JSON-RPC guard refusal (exit code 4).
//!
//! A file body (`body: {file: ...}`) is not read here.

use std::sync::OnceLock;

use rustman_core::run::{JsonrpcRecord, RequestRecord, RpcCallRecord, RpcRefusal};
use rustman_core::vars::ResolvedBody;
use serde_json::{Value, json};

/// The refusal message for calling a method with its params; `None` lets
/// the call through.
pub type MethodCheck = fn(&str, Option<&Value>) -> Option<String>;

static CHECK: OnceLock<MethodCheck> = OnceLock::new();

/// Install the method check every http body that is JSON-RPC goes
/// through. The first one installed stays.
pub fn set_jsonrpc_body_check(check: MethodCheck) {
    let _ = CHECK.set(check);
}

/// A JSON-RPC method in an http body that the check refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyRefusal {
    pub method: String,
    pub message: String,
    pub batch: bool,
}

/// The first refused method of `body`, when it is a JSON-RPC call or
/// batch and a check is installed.
pub fn check_body(body: &ResolvedBody) -> Option<BodyRefusal> {
    let check = CHECK.get()?;
    check_with(body, *check)
}

fn check_with(body: &ResolvedBody, check: MethodCheck) -> Option<BodyRefusal> {
    let parsed;
    let value = match body {
        ResolvedBody::Json(value) => value,
        ResolvedBody::JsonText(text) | ResolvedBody::Text(text) => {
            parsed = serde_json::from_str::<Value>(text.text.trim()).ok()?;
            &parsed
        }
        _ => return None,
    };
    let (calls, batch) = match value {
        Value::Array(items) => (items.iter().collect::<Vec<_>>(), true),
        call => (vec![call], false),
    };
    calls.into_iter().find_map(|call| {
        let method = call.get("method")?.as_str()?;
        let message = check(method, call.get("params"))?;
        Some(BodyRefusal {
            method: method.to_owned(),
            message,
            batch,
        })
    })
}

/// `record` errored with the refusal, its `jsonrpc` part a guard refusal.
pub(super) fn refused(record: RequestRecord, refusal: &BodyRefusal) -> RequestRecord {
    let mut record = record.errored(refusal.message.clone());
    record.jsonrpc = Some(JsonrpcRecord {
        transport: "http".to_owned(),
        batch: refusal.batch,
        calls: vec![RpcCallRecord {
            method: refusal.method.clone(),
            id: None,
            request: json!({"method": refusal.method}),
            response: None,
            outcome: "failed".to_owned(),
            failure: None,
        }],
        attempts: Vec::new(),
        subscription: None,
        note: None,
        refusal: Some(RpcRefusal::Guard),
        elapsed_ms: 0.0,
    });
    record
}

#[cfg(test)]
mod tests {
    use rustman_core::vars::Resolved;

    use super::*;

    fn sends(method: &str, _: Option<&Value>) -> Option<String> {
        method.starts_with("eth_send").then(|| "refused".to_owned())
    }

    #[test]
    fn json_rpc_bodies_are_checked_single_or_batch() {
        let single = ResolvedBody::Json(
            json!({"jsonrpc": "2.0", "id": 1, "method": "eth_sendTransaction", "params": [{}]}),
        );
        let refusal = check_with(&single, sends).unwrap();
        assert_eq!(refusal.method, "eth_sendTransaction");
        assert!(!refusal.batch);
        let text = r#"[{"jsonrpc":"2.0","id":1,"method":"eth_chainId"},{"jsonrpc":"2.0","id":2,"method":"eth_sendRawTransaction","params":["0x02"]}]"#;
        let batch = ResolvedBody::JsonText(Resolved::literal(text));
        let refusal = check_with(&batch, sends).unwrap();
        assert_eq!(refusal.method, "eth_sendRawTransaction");
        assert!(refusal.batch);
        let raw = ResolvedBody::Text(Resolved::literal(r#"{"method":"eth_sendTransaction"}"#));
        assert!(check_with(&raw, sends).is_some());
        for fine in [
            ResolvedBody::Json(json!({"method": "eth_chainId"})),
            ResolvedBody::Json(json!({"payment": {"method": "eth_sendTransaction"}})),
            ResolvedBody::Text(Resolved::literal("eth_sendTransaction")),
            ResolvedBody::None,
        ] {
            assert_eq!(check_with(&fine, sends), None, "{fine:?}");
        }
    }
}
