//! Drawing the messaging pane's tab content: the Messages tab (consumer
//! status, the messages that fit and the selected one's value), the
//! Compose tab and the YAML tabs.
//!
//! A value is shown pretty-printed when it is JSON, with the schema id
//! when it was decoded from the Schema Registry, as text when it is text,
//! and as a hex dump when it is binary (`{"base64": ...}`). Only the rows
//! that fit are cloned and drawn, so the cost of a frame does not grow
//! with the number of messages.

use base64::Engine;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rustman_core::model::Operation;
use serde_json::Value;

use crate::focus::Focus;
use crate::keymap::actions::ActionId;
use crate::messaging::{BUFFER_BYTES, BUFFER_CAP};
use crate::messaging_pane::{MessagingEditor, MsgTab};
use crate::pane::RenderCtx;
use crate::theme::Token;

/// The most characters of a value a list row shows.
const PREVIEW_CHARS: usize = 200;

/// Bytes per hex dump row.
const HEX_ROW: usize = 16;

pub fn content(frame: &mut Frame, rect: Rect, editor: &MessagingEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    match editor.tab {
        MsgTab::Messages => messages(frame, rect, editor, ctx),
        tab => yaml(frame, rect, editor, tab, focused, ctx),
    }
}

fn row(rect: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(offset), rect.width, height).intersection(rect)
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

/// The status line, the visible rows, and the value of the selected (or
/// newest) message.
fn messages(frame: &mut Frame, rect: Rect, editor: &MessagingEditor, ctx: &RenderCtx) {
    let shown = editor.consumer.shown();
    frame.render_widget(
        Paragraph::new(status_line(editor, shown, ctx)),
        row(rect, 0, 1),
    );
    let body = rect.height.saturating_sub(1);
    if shown == 0 {
        frame.render_widget(
            Paragraph::new(muted(&empty_text(editor, ctx), ctx)),
            row(rect, 1, 1),
        );
        return;
    }
    let detail = if body >= 8 { body / 2 } else { 0 };
    let rows = usize::from(body - detail).max(1);
    let selected = editor.selected.unwrap_or(shown - 1).min(shown - 1);
    let first = (selected + 1).saturating_sub(rows);
    let window = editor.consumer.log().window(first, rows.min(shown - first));
    let lines: Vec<Line> = window
        .iter()
        .enumerate()
        .map(|(i, m)| message_line(m, first + i == selected, ctx))
        .collect();
    frame.render_widget(Paragraph::new(lines), row(rect, 1, body - detail));
    if detail > 0
        && let Some(message) = window.get(selected - first)
    {
        value(frame, row(rect, 1 + body - detail, detail), message, ctx);
    }
}

fn empty_text(editor: &MessagingEditor, ctx: &RenderCtx) -> String {
    let key = send_key(ctx);
    match editor.operation {
        Operation::Produce => format!("{key} produces the messages of the Compose tab."),
        Operation::Consume => format!("No messages yet. {key} starts consuming."),
    }
}

/// The key that sends the request in the effective keymap, as the
/// Response pane's hint names it.
fn send_key(ctx: &RenderCtx) -> String {
    ctx.keymap
        .key(ActionId::RequestSend)
        .unwrap_or_else(|| "Ctrl+R".to_owned())
}

/// `1 message`, `4 messages`.
fn message_count(n: usize) -> String {
    match n {
        1 => "1 message".to_owned(),
        n => format!("{n} messages"),
    }
}

/// `Consuming · 120 messages · p pauses, Esc stops`, paused or full,
/// with how many of the oldest messages were dropped.
fn status_line(editor: &MessagingEditor, shown: usize, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let consumer = &editor.consumer;
    let (total, dropped, full) = {
        let log = consumer.log();
        (log.len(), log.dropped(), log.is_full())
    };
    let (state, token) = if full {
        ("Buffer full", Token::Warn)
    } else if consumer.is_paused() {
        ("Paused", Token::Warn)
    } else if consumer.is_running() {
        ("Consuming", Token::Ok)
    } else {
        ("Stopped", Token::Muted)
    };
    let mut counts = format!(" · {}", message_count(shown));
    if shown < total {
        counts.push_str(&format!(" ({} more since the pause)", total - shown));
    }
    if dropped > 0 {
        let mib = BUFFER_BYTES / (1024 * 1024);
        counts.push_str(&format!(" · {dropped} oldest dropped (over {mib} MiB)"));
    }
    if full {
        counts.push_str(&format!(" · the consumer stopped at {BUFFER_CAP}"));
    }
    let hint = if consumer.is_running() {
        " · p pauses, Esc stops".to_owned()
    } else {
        format!(" · {} starts", send_key(ctx))
    };
    Line::from(vec![
        Span::styled(format!(" {state}"), theme.fg(token)),
        Span::styled(counts, theme.fg(Token::Text)),
        Span::styled(hint, theme.fg(Token::Muted)),
    ])
}

/// `p0@42  key=order-1  {"id":1}`, `sensors/a  21.5`.
fn message_line(message: &Value, selected: bool, ctx: &RenderCtx) -> Line<'static> {
    let preview: String = compact(message.get("value"))
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(PREVIEW_CHARS)
        .collect();
    let key = message
        .get("key")
        .filter(|k| !k.is_null())
        .map(|k| format!("key={}  ", compact(Some(k))))
        .unwrap_or_default();
    let text = format!(" {:<24} {key}{preview}", origin(message));
    let style = if selected {
        ctx.ui.theme.selection()
    } else {
        ctx.ui.theme.fg(Token::Text)
    };
    Line::styled(text, style)
}

/// Where `message` came from: `p0@42` (Kafka), its topic (MQTT) or its
/// routing key (AMQP).
fn origin(message: &Value) -> String {
    if let (Some(partition), Some(offset)) = (message.get("partition"), message.get("offset")) {
        return format!("p{partition}@{offset}");
    }
    let topic = message.get("topic").or_else(|| message.get("routing_key"));
    topic.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// A value on one line: text as is, anything else as JSON.
fn compact(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "null".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// The selected message's value, rendered for its kind.
fn value(frame: &mut Frame, rect: Rect, message: &Value, ctx: &RenderCtx) {
    let (what, text) = value_text(message);
    let mut lines = vec![muted(&what, ctx)];
    let style = ctx.ui.theme.fg(Token::Text);
    lines.extend(
        text.lines()
            .take(usize::from(rect.height))
            .map(|l| Line::styled(format!(" {l}"), style)),
    );
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rect);
}

/// What the value is (the header line) and how it is shown.
pub fn value_text(message: &Value) -> (String, String) {
    let size = message.get("size").and_then(Value::as_u64).unwrap_or(0);
    let truncated = if message.get("truncated") == Some(&Value::Bool(true)) {
        ", truncated"
    } else {
        ""
    };
    let value = message.get("value").unwrap_or(&Value::Null);
    let (kind, text) = match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("base64") => (
            "binary".to_owned(),
            hex_dump(map["base64"].as_str().unwrap_or_default()),
        ),
        Value::String(text) => ("text".to_owned(), text.clone()),
        other => (
            json_kind(message),
            serde_json::to_string_pretty(other).unwrap_or_default(),
        ),
    };
    (format!("Value ({kind}, {size} B{truncated})"), text)
}

/// `JSON`, or `decoded from Schema Registry id 7`.
fn json_kind(message: &Value) -> String {
    match message.get("schema_id").and_then(Value::as_u64) {
        Some(id) => format!("decoded from Schema Registry id {id}"),
        None => "JSON".to_owned(),
    }
}

/// `data` (base64) as a hex dump: offset, 16 bytes in hex, then ASCII.
pub fn hex_dump(data: &str) -> String {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return data.to_owned();
    };
    bytes
        .chunks(HEX_ROW)
        .enumerate()
        .map(|(i, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|b| {
                    if b.is_ascii_graphic() {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            format!("{:08x}  {:<47}  {ascii}", i * HEX_ROW, hex.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A YAML tab: what it holds, then its text.
fn yaml(
    frame: &mut Frame,
    rect: Rect,
    editor: &MessagingEditor,
    tab: MsgTab,
    focused: bool,
    ctx: &RenderCtx,
) {
    let what = match tab {
        MsgTab::Compose => "Messages (YAML): a list of {key, value, headers}; Ctrl+R produces them",
        MsgTab::Broker => "Destination and connection (YAML): topic, exchange, queue, auth, tls...",
        _ => "Assertions (YAML): messages, json, body, header...",
    };
    frame.render_widget(Paragraph::new(muted(what, ctx)), row(rect, 0, 1));
    let height = rect.height.saturating_sub(1);
    let Some(area) = editor.area(tab) else {
        return;
    };
    let lines = if area.is_empty() && !focused {
        vec![muted("Empty.", ctx)]
    } else {
        crate::http_view::text(area, height, focused, ctx)
    };
    frame.render_widget(Paragraph::new(lines), row(rect, 1, height));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn values_render_as_json_schema_registry_text_or_hex() {
        let (what, text) = value_text(&json!({"value": {"id": 1}, "size": 8}));
        assert_eq!(what, "Value (JSON, 8 B)");
        assert_eq!(text, "{\n  \"id\": 1\n}");
        let (what, _) = value_text(&json!({"value": {"id": 1}, "size": 9, "schema_id": 7}));
        assert_eq!(what, "Value (decoded from Schema Registry id 7, 9 B)");
        let (what, text) = value_text(&json!({"value": "hi", "size": 2, "truncated": true}));
        assert_eq!(
            (what.as_str(), text.as_str()),
            ("Value (text, 2 B, truncated)", "hi")
        );
        let (what, text) = value_text(&json!({"value": {"base64": "AAFo/w=="}, "size": 4}));
        assert_eq!(what, "Value (binary, 4 B)");
        assert_eq!(
            text,
            "00000000  00 01 68 ff                                      ..h."
        );
    }
}
