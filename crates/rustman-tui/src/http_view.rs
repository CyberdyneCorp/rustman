//! Drawing the `http` pane's tab content: key/value tables, the body by
//! mode, the auth form and the assertions text, with `{{variables}}`
//! highlighted.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::focus::Focus;
use crate::http_pane::{AuthKind, BodyMode, HttpEditor, HttpTab};
use crate::kv::{Cell, KvTable};
use crate::pane::RenderCtx;
use crate::textarea::TextArea;
use crate::theme::Token;
use crate::vars_view::highlight;

/// Widest name column in a table.
const NAME_WIDTH: usize = 24;

pub fn content(frame: &mut Frame, rect: Rect, editor: &HttpEditor, ctx: &RenderCtx) {
    let focused = ctx.focus == Focus::EditorBody;
    let lines = match editor.tab {
        HttpTab::Params => table(&editor.params, rect, focused, ctx),
        HttpTab::Headers => table(&editor.headers, rect, focused, ctx),
        HttpTab::Body => body(editor, rect, focused, ctx),
        HttpTab::Auth => auth(editor, rect, focused, ctx),
        HttpTab::Assertions => {
            let mut lines = text(&editor.assertions, rect.height, focused, ctx);
            if editor.assertions.is_empty() && !focused {
                lines = vec![muted(
                    "No assertions. Type them as YAML, e.g. - status: 200",
                    ctx,
                )];
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines), rect);
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

/// A mode line (`Mode: raw (JSON)  Alt+M change`).
fn mode_line(label: &str, value: &str, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    Line::from(vec![
        Span::styled(format!(" {label}: "), theme.fg(Token::Muted)),
        Span::styled(
            value.to_owned(),
            theme.fg(Token::Accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Alt+M change".to_owned(), theme.fg(Token::Muted)),
    ])
}

fn body(editor: &HttpEditor, rect: Rect, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let mut lines = vec![mode_line("Mode", editor.body_mode.label(), ctx)];
    let rest = Rect {
        height: rect.height.saturating_sub(1),
        ..rect
    };
    match editor.body_mode {
        BodyMode::None => lines.push(muted("No body.", ctx)),
        BodyMode::File => {
            let path = editor.body_file().unwrap_or_default();
            lines.push(muted(&format!("Sends the file {path}"), ctx));
        }
        mode if mode.is_raw() => lines.extend(text(&editor.body, rest.height, focused, ctx)),
        _ => lines.extend(table(&editor.form, rest, focused, ctx)),
    }
    lines
}

fn auth(editor: &HttpEditor, rect: Rect, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let mut lines = vec![mode_line("Type", editor.auth_kind.label(), ctx)];
    let rest = Rect {
        height: rect.height.saturating_sub(1),
        ..rect
    };
    match editor.auth_kind {
        AuthKind::Inherit => lines.push(muted("Uses the nearest collection's auth.", ctx)),
        AuthKind::None => lines.push(muted("No auth is sent.", ctx)),
        AuthKind::OAuth2 => {
            let what = editor.oauth2().map_or_else(String::new, |o| {
                format!(
                    "OAuth2 {} via {}. Edit it in the file.",
                    o.grant.as_str(),
                    o.token_url
                )
            });
            lines.push(muted(&what, ctx));
        }
        AuthKind::Other(name) => {
            lines.push(muted(&format!("{name} auth. Edit it in the file."), ctx));
        }
        _ => lines.extend(table(&editor.auth, rest, focused, ctx)),
    }
    lines
}

/// A text area scrolled to its cursor; the cursor shows while focused.
pub fn text(area: &TextArea, height: u16, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let cursor = area.cursor();
    let rows = usize::from(height).max(1);
    let skip = (cursor.row + 1).saturating_sub(rows);
    let style = ctx.ui.theme.fg(Token::Text);
    area.lines()
        .iter()
        .enumerate()
        .skip(skip)
        .take(rows)
        .map(|(row, line)| {
            let at = (focused && row == cursor.row).then_some(cursor.col);
            let mut line = highlight(line, style, at, ctx.vars, ctx.ui);
            line.spans.insert(0, Span::raw(" "));
            line
        })
        .collect()
}

fn table(table: &KvTable, rect: Rect, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    if table.rows.is_empty() {
        let hint = if table.fixed {
            "No fields."
        } else {
            "No rows. a add"
        };
        return vec![muted(hint, ctx)];
    }
    let width = table
        .rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(4, NAME_WIDTH);
    let rows = usize::from(rect.height).max(1);
    let skip = (table.selected + 1).saturating_sub(rows);
    let mut lines: Vec<Line<'static>> = table
        .rows
        .iter()
        .enumerate()
        .skip(skip)
        .take(rows)
        .map(|(i, row)| {
            let selected = focused && i == table.selected;
            let edit = table.edit.as_ref().filter(|_| selected);
            let theme = &ctx.ui.theme;
            let base = if row.enabled {
                theme.fg(Token::Text)
            } else {
                theme.fg(Token::Muted).add_modifier(Modifier::CROSSED_OUT)
            };
            let mark = match (table.fixed, row.enabled) {
                (true, _) => "",
                (false, true) => "[x] ",
                (false, false) => "[ ] ",
            };
            let pointer = if selected { ctx.ui.glyphs.pointer } else { " " };
            let mut spans = vec![Span::styled(format!(" {pointer}{mark}"), base)];
            let cell = |c: Cell, text: &str| match edit.filter(|e| e.cell == c) {
                Some(e) => highlight(
                    &e.text.text(),
                    base,
                    Some(e.text.cursor().col),
                    ctx.vars,
                    ctx.ui,
                ),
                None => highlight(text, base, None, ctx.vars, ctx.ui),
            };
            let name = cell(Cell::Name, &row.name);
            let pad = width.saturating_sub(row.name.chars().count());
            spans.extend(name.spans);
            spans.push(Span::styled(" ".repeat(pad + 2), base));
            spans.extend(cell(Cell::Value, &row.value).spans);
            let line = Line::from(spans);
            if selected && edit.is_none() {
                line.style(theme.selection())
            } else {
                line
            }
        })
        .collect();
    if focused && table.edit.is_none() {
        let dot = ctx.ui.glyphs.dot;
        let keys = if table.fixed {
            "Enter edit".to_owned()
        } else {
            format!("Enter edit {dot} Space toggle {dot} a add {dot} x remove")
        };
        if lines.len() < rows {
            lines.push(muted(&keys, ctx));
        }
    }
    lines
}
