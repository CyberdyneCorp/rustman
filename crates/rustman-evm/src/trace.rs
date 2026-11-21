//! Call traces (evm-toolkit "Call traces when supported", design D7):
//! `debug_traceCall` with the `callTracer`, else `trace_call` (Erigon and
//! Nethermind style), else a note that tracing is not supported. Both
//! shapes normalize into one [`CallFrame`] tree. Which method works is
//! remembered per endpoint for the session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use alloy_json_abi::JsonAbi;
use alloy_primitives::{Address, U256, hex};
use serde_json::{Value, json};

use crate::call::CallRequest;
use crate::codec::{decode_input, decode_revert, parse_bytes};
use crate::error::EvmError;
use crate::identity::NodeClient;
use crate::quantity::parse_hex;
use crate::rpc::{Rpc, RpcFailure};

/// One call of a trace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallFrame {
    /// `CALL`, `STATICCALL`, `DELEGATECALL`, `CREATE`, ...
    pub kind: String,
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: Option<U256>,
    pub gas: Option<U256>,
    pub gas_used: Option<U256>,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
    /// The node's error for this frame, such as `execution reverted`.
    pub error: Option<String>,
    pub calls: Vec<CallFrame>,
    /// Frames nested below [`MAX_TRACE_DEPTH`] under this one, dropped
    /// rather than kept: rendered as `… N deeper frames omitted`.
    pub omitted: usize,
}

/// The trace methods, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceMethod {
    DebugTraceCall,
    TraceCall,
}

impl TraceMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DebugTraceCall => "debug_traceCall",
            Self::TraceCall => "trace_call",
        }
    }
}

/// What asking for a trace gave.
#[derive(Clone, Debug, PartialEq)]
pub enum TraceResult {
    Tree(Box<CallFrame>),
    /// Neither method exists: `tracing not supported by <client>`.
    Unsupported(String),
    /// A method exists but failed; the call result still stands.
    Failed(String),
}

/// Which trace method each endpoint supports (`None`: neither).
#[derive(Debug, Default)]
pub struct TraceSupport {
    known: Mutex<HashMap<String, Option<TraceMethod>>>,
}

impl TraceSupport {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, endpoint: &str) -> Option<Option<TraceMethod>> {
        self.lock().get(endpoint).copied()
    }

    fn set(&self, endpoint: String, method: Option<TraceMethod>) {
        self.lock().insert(endpoint, method);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<TraceMethod>>> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The note shown when the node has no trace method.
pub fn unsupported_note(client: &NodeClient) -> String {
    format!("tracing not supported by {}", client.kind.as_str())
}

/// Trace `request` on `rpc`. Never an error for the call itself: a node
/// without tracing gives [`TraceResult::Unsupported`].
pub async fn trace_call(
    rpc: &dyn Rpc,
    request: &CallRequest,
    client: &NodeClient,
    support: &TraceSupport,
) -> TraceResult {
    let endpoint = rpc.endpoint();
    let methods = match support.get(&endpoint) {
        Some(None) => return TraceResult::Unsupported(unsupported_note(client)),
        Some(Some(method)) => vec![method],
        None => vec![TraceMethod::DebugTraceCall, TraceMethod::TraceCall],
    };
    for method in methods {
        match rpc.call(method.as_str(), params(method, request)).await {
            Ok(value) => {
                support.set(endpoint, Some(method));
                return parse(method, &value).map_or_else(
                    |e| TraceResult::Failed(e.to_string()),
                    |tree| TraceResult::Tree(Box::new(tree)),
                );
            }
            Err(failure) if unsupported(&failure) => continue,
            Err(failure) => {
                let reason = failure.into_error(method.as_str()).to_string();
                return TraceResult::Failed(reason);
            }
        }
    }
    support.set(endpoint, None);
    TraceResult::Unsupported(unsupported_note(client))
}

fn params(method: TraceMethod, request: &CallRequest) -> Value {
    let tx = request.tx_object();
    let block = request.block.to_param();
    match method {
        TraceMethod::DebugTraceCall => json!([tx, block, {"tracer": "callTracer"}]),
        TraceMethod::TraceCall => json!([tx, ["trace"], block]),
    }
}

/// A node error meaning the method is not there.
fn unsupported(failure: &RpcFailure) -> bool {
    let RpcFailure::Error { message, .. } = failure else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    failure.is_method_not_found()
        || [
            "not supported",
            "does not exist",
            "not available",
            "unsupported",
        ]
        .iter()
        .any(|m| message.contains(m))
}

fn parse(method: TraceMethod, value: &Value) -> Result<CallFrame, EvmError> {
    match method {
        TraceMethod::DebugTraceCall => from_call_tracer(value),
        TraceMethod::TraceCall => {
            let traces = value
                .get("trace")
                .and_then(Value::as_array)
                .ok_or_else(|| EvmError::Rpc("trace_call: no 'trace' list".to_owned()))?;
            from_parity(traces)
        }
    }
}

/// A `callTracer` frame and its nested `calls`. Frames deeper than
/// [`MAX_TRACE_DEPTH`] are counted into the last kept frame's `omitted`.
pub fn from_call_tracer(value: &Value) -> Result<CallFrame, EvmError> {
    call_tracer_frame(value, 0)
}

fn call_tracer_frame(value: &Value, depth: usize) -> Result<CallFrame, EvmError> {
    let children = value
        .get("calls")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let (calls, omitted) = if depth < MAX_TRACE_DEPTH {
        let calls = children
            .iter()
            .map(|call| call_tracer_frame(call, depth + 1))
            .collect::<Result<_, _>>()?;
        (calls, 0)
    } else {
        (Vec::new(), count_call_tracer_frames(children))
    };
    Ok(CallFrame {
        kind: text(value, "type").unwrap_or("CALL").to_ascii_uppercase(),
        from: address(value, "from"),
        to: address(value, "to"),
        value: quantity(value, "value"),
        gas: quantity(value, "gas"),
        gas_used: quantity(value, "gasUsed"),
        input: bytes(value, "input")?,
        output: bytes(value, "output")?,
        error: text(value, "error").map(str::to_owned),
        calls,
        omitted,
    })
}

/// How many frames `calls` holds, at any depth (without recursing).
fn count_call_tracer_frames(calls: &[Value]) -> usize {
    let mut pending: Vec<&Value> = calls.iter().collect();
    let mut count = 0;
    while let Some(call) = pending.pop() {
        count += 1;
        if let Some(nested) = call.get("calls").and_then(Value::as_array) {
            pending.extend(nested);
        }
    }
    count
}

/// The flat `trace_call` list, nested by each entry's `traceAddress`.
pub fn from_parity(traces: &[Value]) -> Result<CallFrame, EvmError> {
    let mut entries = traces
        .iter()
        .map(|t| Ok((trace_address(t), parity_frame(t)?)))
        .collect::<Result<Vec<_>, EvmError>>()?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut entries = entries.into_iter();
    let (root_path, mut root) = entries
        .next()
        .ok_or_else(|| EvmError::Rpc("trace_call: empty trace".to_owned()))?;
    if !root_path.is_empty() {
        return Err(EvmError::Rpc("trace_call: no root frame".to_owned()));
    }
    for (path, frame) in entries {
        insert(&mut root, &path, frame)?;
    }
    Ok(root)
}

/// How deep a trace is kept. The EVM allows 1024 nested calls, but a
/// hostile or broken node could send any depth; frames below this are
/// counted, not kept, which bounds parsing, rendering and dropping.
pub const MAX_TRACE_DEPTH: usize = 256;

fn insert(root: &mut CallFrame, path: &[usize], frame: CallFrame) -> Result<(), EvmError> {
    let Some((last, parents)) = path.split_last() else {
        return Err(EvmError::Rpc("trace_call: two root frames".to_owned()));
    };
    let kept = path.len() <= MAX_TRACE_DEPTH;
    let parents = if kept {
        parents
    } else {
        &path[..MAX_TRACE_DEPTH]
    };
    let mut parent = root;
    for index in parents {
        parent = parent
            .calls
            .get_mut(*index)
            .ok_or_else(|| EvmError::Rpc("trace_call: orphan frame".to_owned()))?;
    }
    if !kept {
        parent.omitted += 1;
        return Ok(());
    }
    if *last != parent.calls.len() {
        return Err(EvmError::Rpc("trace_call: frames out of order".to_owned()));
    }
    parent.calls.push(frame);
    Ok(())
}

fn trace_address(trace: &Value) -> Vec<usize> {
    trace["traceAddress"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .filter_map(|i| usize::try_from(i).ok())
        .collect()
}

fn parity_frame(trace: &Value) -> Result<CallFrame, EvmError> {
    let action = &trace["action"];
    let result = &trace["result"];
    let kind = text(action, "callType").or_else(|| text(trace, "type"));
    Ok(CallFrame {
        kind: kind.unwrap_or("call").to_ascii_uppercase(),
        from: address(action, "from"),
        to: address(action, "to").or_else(|| address(result, "address")),
        value: quantity(action, "value"),
        gas: quantity(action, "gas"),
        gas_used: quantity(result, "gasUsed"),
        input: bytes(action, "input")?,
        output: bytes(result, "output")?,
        error: text(trace, "error").map(str::to_owned),
        calls: Vec::new(),
        omitted: 0,
    })
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn address(value: &Value, key: &str) -> Option<Address> {
    text(value, key).and_then(|s| s.parse().ok())
}

fn quantity(value: &Value, key: &str) -> Option<U256> {
    text(value, key).and_then(|s| parse_hex(s).ok())
}

fn bytes(value: &Value, key: &str) -> Result<Vec<u8>, EvmError> {
    text(value, key).map_or(Ok(Vec::new()), parse_bytes)
}

/// Draws a trace: one line per frame, indented by depth, with the decoded
/// function (from the ABIs known for the frame's address), value, gas and
/// revert reason.
pub struct TraceView<'a> {
    pub abis: &'a dyn Fn(Option<Address>) -> Vec<Arc<JsonAbi>>,
}

impl TraceView<'_> {
    /// Walks the tree with an explicit stack, so a deep trace cannot
    /// overflow the thread's stack.
    pub fn render(&self, root: &CallFrame) -> String {
        let mut lines = Vec::new();
        let mut pending = vec![(root, 0usize)];
        while let Some((frame, depth)) = pending.pop() {
            lines.push(format!("{}{}", "  ".repeat(depth), self.line(frame)));
            if frame.omitted > 0 {
                let indent = "  ".repeat(depth + 1);
                let n = frame.omitted;
                let frames = if n == 1 { "frame" } else { "frames" };
                lines.push(format!("{indent}\u{2026} {n} deeper {frames} omitted"));
            }
            pending.extend(frame.calls.iter().rev().map(|call| (call, depth + 1)));
        }
        lines.join("\n")
    }

    fn line(&self, frame: &CallFrame) -> String {
        let abis = (self.abis)(frame.to);
        let mut parts = vec![
            frame.kind.clone(),
            self.target(frame),
            self.function(frame, &abis),
        ];
        if let Some(value) = frame.value.filter(|v| !v.is_zero()) {
            parts.push(format!("value={value}"));
        }
        if let Some(gas) = frame.gas {
            let used = frame
                .gas_used
                .map(|u| format!(" used={u}"))
                .unwrap_or_default();
            parts.push(format!("gas={gas}{used}"));
        }
        if let Some(error) = &frame.error {
            parts.push(self.revert(frame, error, &abis));
        }
        parts.join(" ")
    }

    fn target(&self, frame: &CallFrame) -> String {
        let shown = |a: Option<Address>| a.map_or("?".to_owned(), |a| a.to_checksum(None));
        format!("{} -> {}", shown(frame.from), shown(frame.to))
    }

    fn function(&self, frame: &CallFrame, abis: &[Arc<JsonAbi>]) -> String {
        let decoded = abis.iter().find_map(|abi| decode_input(abi, &frame.input));
        match decoded {
            Some(call) => call.to_string(),
            None if frame.input.len() >= 4 => hex::encode_prefixed(&frame.input[..4]),
            None => "(no data)".to_owned(),
        }
    }

    fn revert(&self, frame: &CallFrame, error: &str, abis: &[Arc<JsonAbi>]) -> String {
        if frame.output.is_empty() {
            return format!("error: {error}");
        }
        let abi = abis.first().map(AsRef::as_ref);
        decode_revert(abi, &frame.output).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0x00000000000000000000000000000000000000aa";
    const B: &str = "0x00000000000000000000000000000000000000bb";

    #[test]
    fn parity_traces_nest_by_trace_address() {
        let traces = json!([
            {"action": {"callType": "call", "from": A, "to": B, "gas": "0x100", "input": "0x", "value": "0x0"},
             "result": {"gasUsed": "0x10", "output": "0x"}, "traceAddress": [], "type": "call"},
            {"action": {"callType": "staticcall", "from": B, "to": A, "gas": "0x50", "input": "0x"},
             "result": {"gasUsed": "0x5", "output": "0x01"}, "traceAddress": [0], "type": "call"},
            {"action": {"callType": "delegatecall", "from": A, "to": B, "gas": "0x20", "input": "0x"},
             "result": null, "error": "Reverted", "traceAddress": [0, 0], "type": "call"}
        ]);
        let root = from_parity(traces.as_array().unwrap()).unwrap();
        assert_eq!(root.kind, "CALL");
        assert_eq!(root.calls[0].kind, "STATICCALL");
        assert_eq!(root.calls[0].output, vec![1]);
        let inner = &root.calls[0].calls[0];
        assert_eq!(
            (inner.kind.as_str(), inner.error.as_deref()),
            ("DELEGATECALL", Some("Reverted"))
        );
        assert!(from_parity(&[]).is_err());
    }

    fn render_plain(frame: &CallFrame) -> String {
        TraceView {
            abis: &|_| Vec::new(),
        }
        .render(frame)
    }

    #[test]
    fn parity_traces_deeper_than_the_cap_are_counted_not_kept() {
        let frame = |depth: usize| {
            json!({"action": {"callType": "call", "from": A, "to": B, "input": "0x"},
                   "result": {"output": "0x"}, "traceAddress": vec![0; depth], "type": "call"})
        };
        // A hostile node: far deeper than the EVM's own 1024 limit.
        let hostile: Vec<Value> = (0..=MAX_TRACE_DEPTH + 5000).map(frame).collect();
        let root = from_parity(&hostile).unwrap();
        let mut deepest = &root;
        let mut depth = 0;
        while let Some(child) = deepest.calls.first() {
            deepest = child;
            depth += 1;
        }
        assert_eq!((depth, deepest.omitted), (MAX_TRACE_DEPTH, 5000));
        let shown = render_plain(&root);
        assert_eq!(shown.lines().count(), MAX_TRACE_DEPTH + 2);
        let last = shown.lines().last().unwrap();
        assert_eq!(
            last,
            format!(
                "{}\u{2026} 5000 deeper frames omitted",
                "  ".repeat(MAX_TRACE_DEPTH + 1)
            )
        );
    }

    #[test]
    fn call_tracer_traces_deeper_than_the_cap_are_counted_not_kept() {
        let mut value = json!({"type": "CALL", "from": A, "to": B, "input": "0x"});
        for _ in 0..MAX_TRACE_DEPTH + 3 {
            value = json!({"type": "CALL", "from": A, "to": B, "input": "0x", "calls": [value]});
        }
        let root = from_call_tracer(&value).unwrap();
        let shown = render_plain(&root);
        assert_eq!(shown.lines().count(), MAX_TRACE_DEPTH + 2);
        assert!(
            shown.ends_with("\u{2026} 3 deeper frames omitted"),
            "{shown}"
        );
    }

    #[test]
    fn call_tracer_frames_nest() {
        let frame = from_call_tracer(&json!({
            "type": "CALL", "from": A, "to": B, "gas": "0x100", "gasUsed": "0x10",
            "input": "0x", "output": "0x", "value": "0x0",
            "calls": [{"type": "STATICCALL", "from": B, "to": A, "gas": "0x50", "input": "0x"}]
        }))
        .unwrap();
        assert_eq!(frame.calls.len(), 1);
        assert_eq!(frame.gas_used, Some(U256::from(16)));
        let view = TraceView {
            abis: &|_| Vec::new(),
        };
        let shown = view.render(&frame);
        assert_eq!(
            shown,
            "CALL 0x00000000000000000000000000000000000000AA -> 0x00000000000000000000000000000000000000bb (no data) gas=256 used=16\n  STATICCALL 0x00000000000000000000000000000000000000bb -> 0x00000000000000000000000000000000000000AA (no data) gas=80"
        );
    }
}
