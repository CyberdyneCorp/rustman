//! Drawing the realtime pane's tab content: the Timeline tab (session
//! status, filters, the entries that fit and the selected one's payload,
//! pretty-printed when it is JSON), the Compose tab and the YAML tabs.
//!
//! Only the rows that fit are cloned and drawn, so the cost of a frame
//! does not grow with the session's length.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rustman_realtime::{Direction, TimelineEntry};
use serde_json::Value;

use crate::focus::Focus;
use crate::pane::RenderCtx;
use crate::realtime_pane::{RealtimeEditor, RtTab};
use crate::theme::Token;

/// The most characters of a payload a timeline row shows.
const PREVIEW_CHARS: usize = 200;

pub fn content(frame: &mut Frame, rect: Rect, editor: &RealtimeEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    match editor.tab {
        RtTab::Timeline => timeline(frame, rect, editor, ctx),
        RtTab::Compose => compose(frame, rect, editor, focused, ctx),
        tab => yaml(frame, rect, editor, tab, focused, ctx),
    }
}

fn row(rect: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(offset), rect.width, height).intersection(rect)
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

/// The status and filter lines, the visible rows, and the payload of the
/// selected (or newest) entry.
fn timeline(frame: &mut Frame, rect: Rect, editor: &RealtimeEditor, ctx: &RenderCtx) {
    let positions = editor.live.matching(&editor.filter);
    frame.render_widget(
        Paragraph::new(status_line(editor, positions.len(), ctx)),
        row(rect, 0, 1),
    );
    if rect.height < 2 {
        return;
    }
    frame.render_widget(Paragraph::new(filter_line(editor, ctx)), row(rect, 1, 1));
    let body = rect.height.saturating_sub(2);
    let detail = if body >= 8 { body / 3 } else { 0 };
    let rows = usize::from(body - detail);
    if positions.is_empty() {
        frame.render_widget(
            Paragraph::new(muted(empty_text(editor), ctx)),
            row(rect, 2, 1),
        );
        return;
    }
    let selected = editor
        .selected
        .unwrap_or(positions.len() - 1)
        .min(positions.len() - 1);
    let first = (selected + 1).saturating_sub(rows.max(1));
    let window = &positions[first..positions.len().min(first + rows)];
    let entries = editor.live.entries(window);
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| entry_line(e, first + i == selected, ctx))
        .collect();
    frame.render_widget(Paragraph::new(lines), row(rect, 2, body - detail));
    if detail > 0
        && let Some(entry) = entries.get(selected - first)
    {
        payload(frame, row(rect, 2 + body - detail, detail), entry, ctx);
    }
}

fn empty_text(editor: &RealtimeEditor) -> &'static str {
    if editor.filter.is_empty() {
        "No messages yet. Ctrl+R connects."
    } else {
        "No entry matches the filters (c clears them)."
    }
}

/// `Connected · 120 entries (3 dropped)`, or how to connect.
fn status_line(editor: &RealtimeEditor, shown: usize, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let log = editor.live.log();
    let (state, token) = if editor.live.is_connected() {
        ("Connected", Token::Ok)
    } else {
        ("Not connected", Token::Muted)
    };
    let mut counts = format!(" · {shown} of {} entries", log.len());
    if log.dropped() > 0 {
        counts.push_str(&format!(" ({} dropped)", log.dropped()));
    }
    let hint = if editor.live.is_connected() {
        " · Ctrl+R sends, Esc closes"
    } else {
        " · Ctrl+R connects"
    };
    Line::from(vec![
        Span::styled(format!(" {state}"), theme.fg(token)),
        Span::styled(counts, theme.fg(Token::Text)),
        Span::styled(hint, theme.fg(Token::Muted)),
    ])
}

/// `Filter  dir: received  type: all  text: trade_`.
fn filter_line(editor: &RealtimeEditor, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let filter = &editor.filter;
    let dir = filter.dir.map_or("all", Direction::as_str);
    let kind = filter.kind.as_deref().unwrap_or("all");
    let cursor = if editor.editing_filter { "_" } else { "" };
    Line::from(vec![
        Span::styled(" Filter  ", theme.fg(Token::Accent)),
        Span::styled(
            format!("dir: {dir}  type: {kind}  text: {}{cursor}", filter.text),
            theme.fg(Token::Text),
        ),
        Span::styled("  d t / c", theme.fg(Token::Muted)),
    ])
}

/// `   12ms recv json      34B  {"p":1}`.
fn entry_line(entry: &TimelineEntry, selected: bool, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let dir = match entry.dir {
        Direction::Sent => "sent",
        Direction::Received => "recv",
        Direction::System => "sys ",
    };
    let preview: String = entry
        .payload
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(PREVIEW_CHARS)
        .collect();
    let rtt = entry
        .rtt_ms
        .map(|ms| format!(" rtt {ms}ms"))
        .unwrap_or_default();
    let text = format!(
        " {:>6}ms {dir} {:<6} {:>5}B {preview}{rtt}",
        entry.t_ms, entry.kind, entry.size
    );
    let style = if selected {
        theme.selection()
    } else {
        theme.fg(match entry.dir {
            Direction::Sent => Token::Info,
            Direction::Received => Token::Text,
            Direction::System => Token::Muted,
        })
    };
    Line::styled(text, style)
}

/// The payload of `entry`, pretty-printed when it is JSON.
fn payload(frame: &mut Frame, rect: Rect, entry: &TimelineEntry, ctx: &RenderCtx) {
    let text = payload_text(&entry.payload);
    let mut lines = vec![muted(
        &format!("Payload ({} {})", entry.dir.as_str(), entry.kind),
        ctx,
    )];
    let style = ctx.ui.theme.fg(Token::Text);
    lines.extend(
        text.lines()
            .take(usize::from(rect.height))
            .map(|l| Line::styled(format!(" {l}"), style)),
    );
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rect);
}

/// `payload` pretty-printed when it is a JSON object or array; as
/// received otherwise (scalars, text, invalid JSON).
fn payload_text(payload: &str) -> String {
    match serde_json::from_str::<Value>(payload) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => {
            serde_json::to_string_pretty(&value).unwrap_or_default()
        }
        _ => payload.to_owned(),
    }
}

/// The frame type, and the frame as typed.
fn compose(frame: &mut Frame, rect: Rect, editor: &RealtimeEditor, focused: bool, ctx: &RenderCtx) {
    let what = format!(
        "Frame: {} (Alt+M changes the type) · Ctrl+R sends it while connected",
        editor.mode.label()
    );
    frame.render_widget(Paragraph::new(muted(&what, ctx)), row(rect, 0, 1));
    let height = rect.height.saturating_sub(1);
    let lines = if editor.compose.is_empty() && !focused {
        vec![muted("Empty. Type the frame to send.", ctx)]
    } else {
        crate::http_view::text(&editor.compose, height, focused, ctx)
    };
    frame.render_widget(Paragraph::new(lines), row(rect, 1, height));
}

/// A YAML tab: what it holds, then its text.
fn yaml(
    frame: &mut Frame,
    rect: Rect,
    editor: &RealtimeEditor,
    tab: RtTab,
    focused: bool,
    ctx: &RenderCtx,
) {
    let what = match tab {
        RtTab::Headers => "Headers (YAML): a list of {name, value}",
        RtTab::Script if editor.is_ws() => "Steps (YAML): send, wait_for, sleep, close",
        RtTab::Script => "Body (YAML): as for kind: http (POST streams)",
        _ => "Assertions (YAML): messages, close_code, json, body, latency...",
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
    use super::payload_text;

    #[test]
    fn json_payloads_are_pretty_printed_and_others_shown_as_received() {
        assert_eq!(
            payload_text(r#"{"type":"trade","px":[1,2]}"#),
            "{\n  \"type\": \"trade\",\n  \"px\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(payload_text("[]"), "[]");
        for kept in ["42", "\"text\"", "plain text", "{not json", ""] {
            assert_eq!(payload_text(kept), kept);
        }
    }
}
