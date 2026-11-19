//! Drawing the `grpc` pane's tab content: the Method tab (schema line,
//! picked method, the method browser), the Message tab (JSON, with its
//! request type and parse error) and the YAML tabs.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::focus::Focus;
use crate::grpc_pane::{GrpcEditor, GrpcTab, SchemaState};
use crate::pane::RenderCtx;
use crate::theme::Token;

pub fn content(frame: &mut Frame, rect: Rect, editor: &GrpcEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    match editor.tab {
        GrpcTab::Method => browser(frame, rect, editor, focused, ctx),
        GrpcTab::Message => message(frame, rect, editor, focused, ctx),
        tab => yaml(frame, rect, editor, tab, focused, ctx),
    }
}

fn row(rect: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(offset), rect.width, height).intersection(rect)
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

/// Where the schema came from, or why there is none.
pub fn schema_line(editor: &GrpcEditor, ctx: &RenderCtx) -> Line<'static> {
    let (text, token) = match &editor.schema {
        SchemaState::Unloaded | SchemaState::Loading => (
            format!("Schema: loading{}", ctx.ui.glyphs.ellipsis),
            Token::Info,
        ),
        SchemaState::Ready(loaded) if loaded.stale => (
            format!(
                "Schema (may be stale, Ctrl+G refreshes): {} methods from the {}",
                loaded.methods.len(),
                loaded.origin
            ),
            Token::Warn,
        ),
        SchemaState::Ready(loaded) => (
            format!(
                "Schema: {} methods by {} (Ctrl+G refreshes)",
                loaded.methods.len(),
                loaded.origin
            ),
            Token::Muted,
        ),
        SchemaState::Failed(reason) => (format!("No schema: {reason}"), Token::Warn),
    };
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(token))
}

/// The schema line, the picked method, then one row per method.
fn browser(frame: &mut Frame, rect: Rect, editor: &GrpcEditor, focused: bool, ctx: &RenderCtx) {
    let theme = &ctx.ui.theme;
    frame.render_widget(Paragraph::new(schema_line(editor, ctx)), row(rect, 0, 1));
    let call_type = editor
        .call_type()
        .map(|c| format!(" ({c})"))
        .unwrap_or_default();
    let picked = Line::from(vec![
        Span::styled(" Method  ", theme.fg(Token::Accent)),
        Span::styled(
            format!("{}{call_type}", editor.method),
            theme.fg(Token::Text),
        ),
    ]);
    frame.render_widget(Paragraph::new(picked), row(rect, 1, 1));
    let Some(loaded) = editor.loaded() else {
        return;
    };
    let height = usize::from(rect.height.saturating_sub(3));
    let first = (editor.selected + 1).saturating_sub(height.max(1));
    let lines: Vec<Line<'static>> = loaded
        .methods
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, m)| {
            let mark = if m.path == editor.method { "*" } else { " " };
            let text = format!(" {mark} {}  ({})", m.path, m.call_type);
            if focused && i == editor.selected {
                Line::styled(text, theme.selection().add_modifier(Modifier::BOLD))
            } else {
                Line::styled(text, theme.fg(Token::Text))
            }
        })
        .collect();
    let hint = muted("Up/Down select · Enter picks the method", ctx);
    frame.render_widget(Paragraph::new(hint), row(rect, 2, 1));
    let rows = u16::try_from(height).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines), row(rect, 3, rows));
}

/// The request type, the JSON, and its error line.
fn message(frame: &mut Frame, rect: Rect, editor: &GrpcEditor, focused: bool, ctx: &RenderCtx) {
    let request = editor
        .loaded()
        .and_then(|l| l.methods.iter().find(|m| m.path == editor.method))
        .map(|m| format!(" {}", m.request))
        .unwrap_or_default();
    let what = if editor.is_bidi() {
        format!(
            "Message (JSON){request}: a list sends each in order; while the call runs, Ctrl+R sends this tab"
        )
    } else {
        format!("Message (JSON){request}")
    };
    frame.render_widget(Paragraph::new(muted(&what, ctx)), row(rect, 0, 1));
    let height = rect.height.saturating_sub(2);
    let lines = crate::http_view::text(&editor.message, height, focused, ctx);
    frame.render_widget(Paragraph::new(lines), row(rect, 1, height));
    if rect.height > 2
        && let Err(error) = editor.message_value()
    {
        let line = Line::styled(
            format!(" {} {error}", ctx.ui.glyphs.fail),
            ctx.ui.theme.fg(Token::Error),
        );
        frame.render_widget(Paragraph::new(line), row(rect, rect.height - 1, 1));
    }
}

/// A YAML tab: what it holds, then its text.
fn yaml(
    frame: &mut Frame,
    rect: Rect,
    editor: &GrpcEditor,
    tab: GrpcTab,
    focused: bool,
    ctx: &RenderCtx,
) {
    let (area, what) = match tab {
        GrpcTab::Metadata => (
            &editor.metadata,
            "Metadata (YAML): a list of {name, value}; names ending in -bin take base64",
        ),
        _ => (
            &editor.assertions,
            "Assertions (YAML): grpc_status, metadata, json...",
        ),
    };
    frame.render_widget(Paragraph::new(muted(what, ctx)), row(rect, 0, 1));
    let height = rect.height.saturating_sub(1);
    let lines = if area.is_empty() && !focused {
        vec![muted("Empty.", ctx)]
    } else {
        crate::http_view::text(area, height, focused, ctx)
    };
    frame.render_widget(Paragraph::new(lines), row(rect, 1, height));
}
