//! Drawing the `jsonrpc` pane's tab content: the Call tab (catalog line,
//! method field with its completion popup, params), and the YAML tabs.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::focus::Focus;
use crate::jsonrpc_pane::{CallField, CatalogState, JsonrpcEditor, MethodPopup, RpcTab};
use crate::pane::RenderCtx;
use crate::theme::Token;

/// The most completion candidates shown at once.
const POPUP_ROWS: usize = 6;

pub fn content(frame: &mut Frame, rect: Rect, editor: &JsonrpcEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    match editor.tab {
        RpcTab::Call if editor.base().batch.is_some() => {
            let calls = editor.base().batch.as_ref().map_or(0, |b| b.len());
            let text =
                format!("This request is a batch of {calls} calls: edit it in the Batch tab.");
            frame.render_widget(Paragraph::new(muted(&text, ctx)), rect);
        }
        RpcTab::Call => call(frame, rect, editor, focused, ctx),
        tab => yaml(frame, rect, editor, tab, focused, ctx),
    }
}

fn row(rect: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(offset), rect.width, height).intersection(rect)
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

/// The catalog line, the method field, the params and their error line.
fn call(frame: &mut Frame, rect: Rect, editor: &JsonrpcEditor, focused: bool, ctx: &RenderCtx) {
    frame.render_widget(Paragraph::new(catalog_line(editor, ctx)), row(rect, 0, 1));
    let on_method = focused && editor.field == CallField::Method;
    frame.render_widget(
        Paragraph::new(method_line(editor, on_method, ctx)),
        row(rect, 1, 1),
    );
    let label = editor.params_label();
    frame.render_widget(Paragraph::new(muted(&label, ctx)), row(rect, 2, 1));
    let body_height = rect.height.saturating_sub(4);
    let on_params = focused && editor.field == CallField::Params;
    let lines = if editor.params.is_empty() && !on_params {
        vec![muted(
            "No params. Type a JSON array or object, e.g. [\"latest\", false]",
            ctx,
        )]
    } else {
        crate::http_view::text(&editor.params, body_height, on_params, ctx)
    };
    frame.render_widget(Paragraph::new(lines), row(rect, 3, body_height));
    if rect.height > 4
        && let Some(error) = editor.params_error()
    {
        let line = Line::styled(
            format!(" {} {error}", ctx.ui.glyphs.fail),
            ctx.ui.theme.fg(Token::Error),
        );
        frame.render_widget(Paragraph::new(line), row(rect, rect.height - 1, 1));
    }
    if let Some(popup) = editor.popup.as_ref().filter(|_| on_method) {
        draw_popup(
            frame,
            row(rect, 2, rect.height.saturating_sub(2)),
            popup,
            ctx,
        );
    }
}

/// Where completions come from, or why there are none.
pub fn catalog_line(editor: &JsonrpcEditor, ctx: &RenderCtx) -> Line<'static> {
    let (text, token) = match &editor.catalog {
        CatalogState::Unloaded => (
            format!("Catalog: loading{}", ctx.ui.glyphs.ellipsis),
            Token::Info,
        ),
        CatalogState::Ready(Some(catalog)) => (
            format!(
                "Catalog: {} methods (Ctrl+Space completes the method)",
                catalog.methods.len()
            ),
            Token::Muted,
        ),
        CatalogState::Ready(None) => (
            "No catalog: run rustman rpc discover <endpoint> to complete methods".to_owned(),
            Token::Muted,
        ),
        CatalogState::Failed(reason) => (format!("No catalog: {reason}"), Token::Warn),
    };
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(token))
}

/// ` Method  eth_getBalance`, with the cursor when it has the focus.
fn method_line(editor: &JsonrpcEditor, focused: bool, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let mut spans = vec![Span::styled(" Method  ", theme.fg(Token::Accent))];
    let text = editor.method.text();
    if text.is_empty() && !focused {
        let hint = match editor.subscribe_method() {
            Some(subscribe) => format!("{subscribe} (the subscription's subscribe method)"),
            None => "Type a method, e.g. eth_blockNumber".to_owned(),
        };
        spans.push(Span::styled(hint, theme.fg(Token::Muted)));
        return Line::from(spans);
    }
    let style = theme.fg(Token::Text);
    let col = editor.method.cursor().col;
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars.iter().take(col).collect();
    spans.push(Span::styled(before, style));
    if focused {
        let at = chars.get(col).copied().unwrap_or(' ');
        spans.push(Span::styled(
            at.to_string(),
            style.add_modifier(Modifier::REVERSED),
        ));
        spans.push(Span::styled(
            chars.iter().skip(col + 1).collect::<String>(),
            style,
        ));
    } else {
        spans.push(Span::styled(
            chars.iter().skip(col).collect::<String>(),
            style,
        ));
    }
    Line::from(spans)
}

/// The candidates under the method field, the selected one highlighted.
fn draw_popup(frame: &mut Frame, area: Rect, popup: &MethodPopup, ctx: &RenderCtx) {
    let theme = &ctx.ui.theme;
    let first = (popup.selected + 1).saturating_sub(POPUP_ROWS);
    let shown: Vec<(usize, String)> = popup
        .items
        .iter()
        .enumerate()
        .skip(first)
        .take(POPUP_ROWS)
        .map(|(i, item)| (i, format!(" {}({}) ", item.name, item.params)))
        .collect();
    let widest = shown
        .iter()
        .map(|(_, t)| t.chars().count())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(widest).unwrap_or(u16::MAX).min(area.width);
    let height = u16::try_from(shown.len()).unwrap_or(0);
    let rect = Rect::new(area.x + 9.min(area.width), area.y, width, height).intersection(area);
    let lines: Vec<Line<'static>> = shown
        .into_iter()
        .map(|(i, text)| {
            let style = if i == popup.selected {
                theme.selection().add_modifier(Modifier::BOLD)
            } else {
                theme
                    .fg(Token::Text)
                    .bg(theme.color(Token::Selection).unwrap_or_default())
            };
            Line::styled(text, style)
        })
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines), rect);
}

/// A YAML tab: what it holds, then its text.
fn yaml(
    frame: &mut Frame,
    rect: Rect,
    editor: &JsonrpcEditor,
    tab: RpcTab,
    focused: bool,
    ctx: &RenderCtx,
) {
    let (area, what) = match tab {
        RpcTab::Batch => (
            &editor.batch,
            "Batch (YAML): a list of {method, params, id, assertions}",
        ),
        RpcTab::Headers => (&editor.headers, "Headers (YAML): a list of {name, value}"),
        RpcTab::Auth => (&editor.auth, "Auth (YAML): as for kind: http"),
        _ => (
            &editor.assertions,
            "Assertions (YAML): status, header, json, rpc_error...",
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
