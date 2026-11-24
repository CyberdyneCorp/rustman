//! The summary line of a request that finished without an HTTP exchange:
//! an `evm-call`, a Kafka/MQTT/AMQP produce or consume, a JSON-RPC call
//! over WebSocket or IPC, an MCP operation, a WebSocket/SSE session or a
//! request of a plugin kind. Their result lives in the kind's own record,
//! so the line reads the record's status and the kind's summary rather
//! than an HTTP status.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::run::{RequestRecord, RequestStatus};
use serde_json::Value;

use super::timing::ms_text;
use crate::config::UiOptions;
use crate::theme::Token;

/// `✔ OK · balanceOf(address) · Ethereum (1) · geth 1.14 · 12 ms · 1/1
/// passed` for a request that ran to completion without an HTTP exchange;
/// `None` when it errored or was skipped (the failure line says why).
pub fn line(record: &RequestRecord, ui: &UiOptions) -> Option<Line<'static>> {
    let (title, token) = match record.status {
        RequestStatus::Passed => (format!("{} OK", ui.glyphs.ok), Token::Ok),
        RequestStatus::Failed => (format!("{} Failed", ui.glyphs.fail), Token::Error),
        RequestStatus::Errored | RequestStatus::Skipped => return None,
    };
    let mut parts = summary(record);
    parts.retain(|p| !p.is_empty());
    parts.push(ms_text(record.duration_ms));
    parts.extend(super::view::tally(record));
    if record.status == RequestStatus::Failed {
        parts.extend(record.error.clone());
    }
    let dot = ui.glyphs.dot;
    let rest: String = parts.iter().map(|p| format!(" {dot} {p}")).collect();
    Some(Line::from(vec![
        Span::styled(title, ui.theme.fg(token).add_modifier(Modifier::BOLD)),
        Span::styled(rest, ui.theme.fg(Token::Text)),
    ]))
}

/// What the kind's own record says about the result, most telling first.
fn summary(record: &RequestRecord) -> Vec<String> {
    if let Some(evm) = &record.evm {
        return evm_summary(evm);
    }
    if let Some(m) = &record.messaging {
        return vec![format!("{} {}", m.protocol, m.operation)];
    }
    if let Some(mcp) = &record.mcp {
        return vec![
            format!("{} {}", mcp.server, mcp.method),
            mcp.outcome.clone(),
        ];
    }
    if let Some(rpc) = &record.jsonrpc {
        let methods: Vec<&str> = rpc.calls.iter().map(|c| c.method.as_str()).collect();
        return vec![methods.join(", "), rpc.transport.clone()];
    }
    if let Some(rt) = &record.realtime {
        return vec![format!("{} · {} received", rt.protocol, rt.received)];
    }
    if let Some(plugin) = &record.plugin_kind {
        let mut parts = vec![plugin.kind.clone(), plugin.summary.clone()];
        parts.extend(plugin.status.map(|s| s.to_string()));
        return parts;
    }
    Vec::new()
}

/// The decoded function (or its revert) and the chain and client answered.
fn evm_summary(evm: &Value) -> Vec<String> {
    let result = &evm["result"];
    let mut parts = Vec::new();
    if let Some(function) = result["function"].as_str() {
        parts.push(function.to_owned());
    }
    if let Some(revert) = result["reverted"]["message"].as_str() {
        parts.push(format!("reverted: {revert}"));
    }
    if let Some(chain) = evm["status"].as_str() {
        parts.push(chain.to_owned());
    }
    parts
}
