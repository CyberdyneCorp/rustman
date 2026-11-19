//! The JSON-RPC result in the response viewer (add-jsonrpc-client "TUI
//! JSON-RPC pane"): the `Result`, `Error` and `Events` tabs, prepared off
//! the render loop from the redacted JSON-RPC record. Values are shown
//! exactly as received: hex stays hex.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::run::{JsonrpcRecord, RpcCallRecord};
use serde_json::Value;

use crate::config::UiOptions;
use crate::theme::Token;
use crate::view::hint;

/// One call's error: the code and message, then its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcErrorRow {
    /// `#2 eth_call` in a batch, empty for a single call.
    pub call: String,
    /// `rpc error -32601: Method not found`, or why no answer came.
    pub message: String,
    /// The error's `data`, pretty-printed.
    pub data: Vec<String>,
}

/// A JSON-RPC exchange as the viewer shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RpcView {
    /// `call eth_blockNumber · result`, `batch of 3 · 1 error`...
    pub summary: String,
    /// Per call: its heading (in a batch) and its pretty result.
    pub results: Vec<(String, Vec<String>)>,
    pub errors: Vec<RpcErrorRow>,
    /// One line per subscription event.
    pub events: Vec<String>,
    /// Why the subscription ended, and how many events it got in all.
    pub ended: Option<(String, u64)>,
    /// `server rejected the batch`.
    pub note: Option<String>,
}

impl RpcView {
    pub fn new(record: &JsonrpcRecord) -> Self {
        let batch = record.batch;
        let heading = |i: usize, c: &RpcCallRecord| {
            if batch {
                format!("#{} {}", i + 1, c.method)
            } else {
                String::new()
            }
        };
        let mut view = Self {
            summary: summary(record),
            note: record.note.clone(),
            ..Self::default()
        };
        for (i, call) in record.calls.iter().enumerate() {
            let answer = call.response.as_ref();
            if let Some(result) = answer.and_then(|r| r.get("result")) {
                view.results.push((heading(i, call), pretty(result)));
            }
            if let Some(message) = &call.failure {
                let data = answer
                    .and_then(|r| r.pointer("/error/data"))
                    .map(pretty)
                    .unwrap_or_default();
                view.errors.push(RpcErrorRow {
                    call: heading(i, call),
                    message: message.clone(),
                    data,
                });
            }
        }
        if let Some(sub) = &record.subscription {
            view.events = sub.events.iter().map(Value::to_string).collect();
            view.ended = Some((sub.ended.clone(), sub.event_count));
        }
        view
    }
}

/// `subscription eth_subscribe · ws · stopped`, `batch of 2 · 1 error`, or
/// `call eth_blockNumber · result`.
fn summary(record: &JsonrpcRecord) -> String {
    let dot = " · ";
    if let Some(sub) = &record.subscription {
        let method = record.calls.first().map_or("", |c| c.method.as_str());
        return format!(
            "subscription {method}{dot}{}{dot}{}",
            record.transport, sub.ended
        );
    }
    let failed = record.calls.iter().filter(|c| c.failure.is_some()).count();
    let outcome = match failed {
        0 => "ok".to_owned(),
        1 => "1 error".to_owned(),
        n => format!("{n} errors"),
    };
    match record.calls.as_slice() {
        [one] if !record.batch => format!("call {}{dot}{}", one.method, one.outcome),
        calls => format!("batch of {}{dot}{outcome}", calls.len()),
    }
}

fn pretty(value: &Value) -> Vec<String> {
    serde_json::to_string_pretty(value)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The lines of the JSON-RPC tab `label` (`Result`, `Error`, `Events`),
/// under the view's summary.
pub fn lines(view: &RpcView, label: &str, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        view.summary.clone(),
        ui.theme.fg(Token::Muted),
    )];
    lines.extend(match label {
        "Result" => result_lines(view, ui),
        "Error" => error_lines(view, ui),
        _ => event_lines(view, ui),
    });
    lines
}

fn result_lines(view: &RpcView, ui: &UiOptions) -> Vec<Line<'static>> {
    if view.results.is_empty() {
        return vec![hint("No result: see the Error tab.", ui)];
    }
    let mut lines = Vec::new();
    for (heading, result) in &view.results {
        if !heading.is_empty() {
            lines.push(Line::styled(heading.clone(), ui.theme.fg(Token::Accent)));
        }
        lines.extend(
            result
                .iter()
                .map(|l| Line::styled(l.clone(), ui.theme.fg(Token::Text))),
        );
    }
    lines
}

fn error_lines(view: &RpcView, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(note) = &view.note {
        lines.push(Line::styled(note.clone(), ui.theme.fg(Token::Warn)));
    }
    if view.errors.is_empty() {
        lines.push(hint("No errors.", ui));
        return lines;
    }
    for error in &view.errors {
        let call = if error.call.is_empty() {
            String::new()
        } else {
            format!("{} ", error.call)
        };
        lines.push(Line::styled(
            format!("{} {call}{}", ui.glyphs.fail, error.message),
            ui.theme.fg(Token::Error).add_modifier(Modifier::BOLD),
        ));
        if !error.data.is_empty() {
            lines.push(Line::styled("  data:", ui.theme.fg(Token::Muted)));
            lines.extend(
                error
                    .data
                    .iter()
                    .map(|l| Line::styled(format!("  {l}"), ui.theme.fg(Token::Text))),
            );
        }
    }
    lines
}

fn event_lines(view: &RpcView, ui: &UiOptions) -> Vec<Line<'static>> {
    let Some((reason, count)) = &view.ended else {
        return vec![hint("No events: this is not a subscription.", ui)];
    };
    let mut lines: Vec<Line<'static>> = view
        .events
        .iter()
        .enumerate()
        .map(|(i, payload)| {
            Line::from(vec![
                Span::styled(format!("#{:<3} ", i + 1), ui.theme.fg(Token::Accent)),
                Span::styled(payload.clone(), ui.theme.fg(Token::Text)),
            ])
        })
        .collect();
    let kept = if *count > view.events.len() as u64 {
        format!(" ({} of {count} kept)", view.events.len())
    } else {
        String::new()
    };
    lines.push(hint(&format!("Ended: {reason}, {count} events{kept}"), ui));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::run::RpcSubscriptionRecord;
    use serde_json::json;

    fn call(method: &str, response: Value, failure: Option<&str>) -> RpcCallRecord {
        RpcCallRecord {
            method: method.into(),
            id: Some(json!(1)),
            request: json!({"jsonrpc": "2.0", "method": method, "id": 1}),
            outcome: if failure.is_some() { "error" } else { "result" }.into(),
            response: Some(response),
            failure: failure.map(str::to_owned),
        }
    }

    fn record(batch: bool, calls: Vec<RpcCallRecord>) -> JsonrpcRecord {
        JsonrpcRecord {
            transport: "http".into(),
            batch,
            calls,
            attempts: Vec::new(),
            subscription: None,
            note: None,
            refusal: None,
            elapsed_ms: 1.0,
        }
    }

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_hex_result_is_shown_as_received() {
        // Scenario "Hex left as is".
        let view = RpcView::new(&record(
            false,
            vec![call("eth_blockNumber", json!({"result": "0x10"}), None)],
        ));
        let ui = UiOptions::default();
        let shown = text(&lines(&view, "Result", &ui));
        assert_eq!(shown, "call eth_blockNumber · result\n\"0x10\"");
        assert!(text(&lines(&view, "Error", &ui)).contains("No errors."));
        assert!(text(&lines(&view, "Events", &ui)).contains("not a subscription"));
    }

    #[test]
    fn batch_errors_show_code_message_and_data() {
        let error = json!({"error": {"code": 3, "message": "reverted", "data": "0x08c379a0"}});
        let mut rejected = record(
            true,
            vec![
                call("eth_chainId", json!({"result": "0x1"}), None),
                call("eth_call", error, Some("rpc error 3: reverted")),
            ],
        );
        rejected.note = Some("server rejected the batch".into());
        let view = RpcView::new(&rejected);
        let ui = UiOptions::default();
        assert_eq!(view.summary, "batch of 2 · 1 error");
        let errors = text(&lines(&view, "Error", &ui));
        assert!(errors.contains("server rejected the batch"), "{errors}");
        assert!(
            errors.contains("#2 eth_call rpc error 3: reverted"),
            "{errors}"
        );
        assert!(errors.contains("  \"0x08c379a0\""), "{errors}");
        assert!(text(&lines(&view, "Result", &ui)).contains("#1 eth_chainId\n\"0x1\""));
    }

    #[test]
    fn subscription_events_end_with_why() {
        let mut live = record(
            false,
            vec![call("eth_subscribe", json!({"result": "0xs"}), None)],
        );
        let mut sub = RpcSubscriptionRecord::new(json!("0xs"), 3, vec![json!({"n": 1})]);
        sub.ended = "stopped".into();
        live.subscription = Some(sub);
        live.transport = "ws".into();
        let view = RpcView::new(&live);
        assert_eq!(view.summary, "subscription eth_subscribe · ws · stopped");
        let events = text(&lines(&view, "Events", &UiOptions::default()));
        assert!(events.contains("#1   {\"n\":1}"), "{events}");
        assert!(
            events.contains("Ended: stopped, 3 events (1 of 3 kept)"),
            "{events}"
        );
    }
}
