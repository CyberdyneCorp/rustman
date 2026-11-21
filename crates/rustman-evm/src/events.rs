//! The events a read-only call would emit (evm-toolkit "EVM assertions"
//! `emits`, "EVM captures" `evm_event`). `eth_call` returns no logs, so
//! when a request asks for its events the call is simulated once more
//! with `debug_traceCall` and the `callTracer`'s `withLog` option. Logs of
//! frames that reverted are left out, as the chain would drop them. A node
//! without `debug_traceCall` gives no events and a warning.

use std::sync::Arc;

use alloy_json_abi::JsonAbi;
use alloy_primitives::{Address, B256, Bytes};
use rustman_core::model::{Assertion, Capture, CaptureSource, Checked};
use serde_json::{Map, Value, json};

use crate::call::CallRequest;
use crate::codec::{DecodedLog, decode_log, value_to_json};
use crate::rpc::Rpc;

/// The warning when the node cannot simulate the events.
pub const NO_EVENTS: &str = "events of a call need debug_traceCall";

/// Whether the assertions or captures of a request read its events.
pub fn wants_events(assertions: &[Assertion], captures: &[Checked<Capture>]) -> bool {
    let emits = assertions
        .iter()
        .any(|a| matches!(a, Assertion::Evm(e) if e.emits.is_some()));
    emits || captures.iter().any(|c| c.from == CaptureSource::EvmEvent)
}

/// The decoded events `request` would emit, in frame order: the ABI of
/// the called contract for its own logs, the built-in ABIs for others.
/// Logs no ABI decodes are skipped.
pub async fn simulated_events(
    rpc: &dyn Rpc,
    request: &CallRequest,
    abi: &Arc<JsonAbi>,
) -> Result<Vec<DecodedLog>, String> {
    let params = json!([
        request.tx_object(),
        request.block.to_param(),
        {"tracer": "callTracer", "tracerConfig": {"withLog": true}}
    ]);
    let frame = rpc
        .call("debug_traceCall", params)
        .await
        .map_err(|failure| failure.into_error("debug_traceCall").to_string())?;
    let mut logs = Vec::new();
    collect_logs(&frame, &mut logs);
    let built_in: Vec<Arc<JsonAbi>> = crate::abi::BUILT_IN
        .iter()
        .filter_map(|n| crate::abi::built_in(n))
        .collect();
    Ok(logs
        .iter()
        .filter_map(|log| {
            let abis = match log_address(log) == Some(request.to) {
                true => std::slice::from_ref(abi),
                false => built_in.as_slice(),
            };
            decode_any(abis, log)
        })
        .collect())
}

/// The logs of `frame` and its sub-calls, depth first, skipping frames
/// that reverted (and so their sub-calls).
fn collect_logs<'a>(frame: &'a Value, out: &mut Vec<&'a Value>) {
    if frame.get("error").is_some() {
        return;
    }
    out.extend(items(frame, "logs"));
    for call in items(frame, "calls") {
        collect_logs(call, out);
    }
}

fn items<'a>(frame: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    frame
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn log_address(log: &Value) -> Option<Address> {
    log.get("address")?.as_str()?.parse().ok()
}

fn decode_any(abis: &[Arc<JsonAbi>], log: &Value) -> Option<DecodedLog> {
    let topics: Vec<B256> = log
        .get("topics")?
        .as_array()?
        .iter()
        .map(|t| t.as_str()?.parse().ok())
        .collect::<Option<_>>()?;
    let data: Bytes = log
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or("0x")
        .parse()
        .ok()?;
    abis.iter().find_map(|abi| decode_log(abi, &topics, &data))
}

/// The events as captures read them: `{signature, fields: {name: value}}`.
pub fn events_json(events: &[DecodedLog]) -> Value {
    let shown: Vec<Value> = events
        .iter()
        .map(|log| {
            let fields: Map<String, Value> = log
                .fields
                .iter()
                .map(|f| (f.name.clone(), value_to_json(&f.value)))
                .collect();
            json!({"signature": log.event.signature(), "fields": fields})
        })
        .collect();
    Value::Array(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverted_frames_lose_their_logs() {
        let frame = json!({
            "logs": [{"n": 1}],
            "calls": [
                {"logs": [{"n": 2}], "calls": [{"logs": [{"n": 3}]}]},
                {"error": "execution reverted", "logs": [{"n": 4}],
                 "calls": [{"logs": [{"n": 5}]}]}
            ]
        });
        let mut logs = Vec::new();
        collect_logs(&frame, &mut logs);
        let seen: Vec<i64> = logs.iter().map(|l| l["n"].as_i64().unwrap()).collect();
        assert_eq!(seen, [1, 2, 3]);
    }
}
