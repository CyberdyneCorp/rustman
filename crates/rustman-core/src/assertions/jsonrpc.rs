//! The JSON-RPC check (add-jsonrpc-client "rpc_error assertion"):
//! `rpc_error: {code, message_contains}` passes when the call failed with
//! that error, `rpc_error: none` when it returned a result.
//!
//! It reads the body as a JSON-RPC response envelope: an object with
//! `result` or an `error` object carrying an integer `code`.

use serde_json::Value;

use super::expected::Interp;
use super::response::Parsed;
use super::{AssertionResult, Check, quoted};
use crate::model::RpcErrorExpectation;

/// Why `rpc_error` cannot be evaluated against a body.
const NOT_JSONRPC: &str = "rpc_error: response is not a JSON-RPC response";

/// How the call ended, as the envelope says.
enum Ended<'r> {
    Result,
    Error { code: i64, message: &'r str },
}

impl Ended<'_> {
    fn shown(&self) -> String {
        match self {
            Self::Result => "result".to_owned(),
            Self::Error { code, .. } => format!("code {code}"),
        }
    }
}

/// How the envelope in the body ended, or `None` when it is not one.
fn ended<'r>(response: &'r Parsed<'_, '_>) -> Option<Ended<'r>> {
    let Ok(Value::Object(envelope)) = response.json() else {
        return None;
    };
    if let Some(error) = envelope.get("error").and_then(Value::as_object) {
        let code = error.get("code").and_then(Value::as_i64)?;
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        return Some(Ended::Error { code, message });
    }
    envelope.contains_key("result").then_some(Ended::Result)
}

pub(super) fn evaluate(
    expected: &RpcErrorExpectation,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    match expected {
        RpcErrorExpectation::None => none(response),
        RpcErrorExpectation::Code {
            code,
            message_contains,
        } => with_code(*code, message_contains.as_deref(), response, interp),
    }
}

fn none(response: &Parsed<'_, '_>) -> AssertionResult {
    let check = Check::new("rpc_error", "rpc_error none");
    let Some(ended) = ended(response) else {
        return check.error(NOT_JSONRPC);
    };
    let ok = matches!(ended, Ended::Result);
    check.verdict(ok, "rpc_error", "none".to_owned(), ended.shown())
}

fn with_code(
    code: i64,
    raw: Option<&str>,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let label = match raw {
        Some(text) => format!("rpc_error code {code} message_contains {}", quoted(text)),
        None => format!("rpc_error code {code}"),
    };
    let check = Check::new("rpc_error", label);
    let Some(ended) = ended(response) else {
        return check.error(NOT_JSONRPC);
    };
    let Ended::Error { code: got, message } = ended else {
        return check.fail("rpc_error", format!("code {code}"), ended.shown());
    };
    if got != code {
        return check.fail("rpc_error", format!("code {code}"), ended.shown());
    }
    let Some(raw) = raw else {
        return check.pass();
    };
    let text = match interp.text(raw) {
        Ok(text) => text,
        Err(e) => return check.error(format!("rpc_error: {e}")),
    };
    check.verdict(
        message.contains(&text),
        "rpc_error",
        format!("a message containing {}", quoted(&text)),
        quoted(message),
    )
}
