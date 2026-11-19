//! Printing JSON-RPC results. Everything printed comes from the request
//! record, which the runner has already redacted, so no secret reaches
//! the terminal. Values are shown exactly as received (`0x10` stays
//! `0x10`).

use std::io::Write;

use rustman_core::ConfigError;
use rustman_core::run::{JsonrpcRecord, RequestRecord, RpcCallRecord};
use serde_json::{Value, json};

/// How results are printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The result (or the error object) as pretty JSON; subscription
    /// events one per line.
    Pretty,
    /// Each response envelope (and event message) as received, one per line.
    Raw,
    /// The whole exchange as one JSON object.
    Json,
}

/// What a batch call answered: its `result`, or its `error` object as
/// `{"error": {...}}`; `None` for a notification or no response.
pub fn answer(call: &RpcCallRecord) -> Option<Value> {
    let response = call.response.as_ref()?;
    if let Some(error) = response.get("error") {
        return Some(json!({ "error": error }));
    }
    response.get("result").cloned()
}

/// Print the outcome of `rustman rpc call`.
pub fn print_call(record: &RequestRecord, format: Format) -> Result<(), ConfigError> {
    let Some(jsonrpc) = &record.jsonrpc else {
        return Ok(());
    };
    let mut out = Vec::new();
    match format {
        Format::Json => {
            let whole = json!({
                "status": record.status,
                "error": record.error,
                "jsonrpc": jsonrpc,
                "assertions": record.assertions,
            });
            out.push(pretty(&whole));
        }
        Format::Raw => out.extend(raw_lines(jsonrpc)),
        Format::Pretty => out.extend(pretty_lines(jsonrpc)),
    }
    write_lines(&out)
}

fn pretty_lines(jsonrpc: &JsonrpcRecord) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for call in &jsonrpc.calls {
        let response = call.response.as_ref();
        let shown = response.and_then(|r| r.get("error").or_else(|| r.get("result")));
        if let Some(shown) = shown {
            lines.push(pretty(shown));
        }
    }
    if let Some(subscription) = &jsonrpc.subscription {
        lines.extend(subscription.events.iter().map(Value::to_string));
    }
    lines
}

fn raw_lines(jsonrpc: &JsonrpcRecord) -> Vec<String> {
    let mut lines: Vec<String> = jsonrpc
        .calls
        .iter()
        .filter_map(|c| c.response.as_ref())
        .map(Value::to_string)
        .collect();
    if let Some(subscription) = &jsonrpc.subscription {
        lines.extend(subscription.events.iter().map(Value::to_string));
    }
    lines
}

/// `value` as indented JSON.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Write `lines` to stdout; a closed stdout is not an error worth a panic.
pub fn write_lines(lines: &[String]) -> Result<(), ConfigError> {
    let mut out = std::io::stdout().lock();
    for line in lines {
        if let Err(e) = writeln!(out, "{line}") {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(ConfigError::Io {
                path: "<stdout>".into(),
                source: e,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(response: Value) -> RpcCallRecord {
        RpcCallRecord {
            method: "m".into(),
            id: Some(json!(1)),
            request: json!({}),
            response: Some(response),
            outcome: "result".into(),
            failure: None,
        }
    }

    #[test]
    fn answers_are_the_result_or_the_error_object() {
        let ok = call(json!({"jsonrpc":"2.0","id":1,"result":"0x10"}));
        assert_eq!(answer(&ok), Some(json!("0x10")));
        let failed = call(
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}),
        );
        assert_eq!(
            answer(&failed),
            Some(json!({"error": {"code": -32601, "message": "Method not found"}}))
        );
    }
}
