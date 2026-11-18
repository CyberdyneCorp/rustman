//! Drawing the `graphql` pane's tab content: the document with syntax
//! colours, underlined diagnostics and the completion popup; the variables;
//! and the schema explorer.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use rustman_graphql::{Diagnostic, Severity, TokenClass, TypeDetail, TypeKind, highlight};

use crate::focus::Focus;
use crate::graphql_pane::{GqlTab, GraphqlEditor, Popup, SchemaState};
use crate::pane::RenderCtx;
use crate::theme::Token;

/// The most completion candidates shown at once.
const POPUP_ROWS: usize = 6;

pub fn content(frame: &mut Frame, rect: Rect, editor: &GraphqlEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    match editor.tab {
        GqlTab::Query => query(frame, rect, editor, focused, ctx),
        GqlTab::Variables => variables(frame, rect, editor, focused, ctx),
        GqlTab::Schema => {
            let lines = explorer(editor, rect.height, focused, ctx);
            frame.render_widget(Paragraph::new(lines), rect);
        }
    }
}

fn row(rect: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(offset), rect.width, height).intersection(rect)
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

// ---- Query -------------------------------------------------------------

/// The schema line, the document, then the diagnostics line.
fn query(frame: &mut Frame, rect: Rect, editor: &GraphqlEditor, focused: bool, ctx: &RenderCtx) {
    frame.render_widget(Paragraph::new(schema_line(editor, ctx)), row(rect, 0, 1));
    let body_height = rect.height.saturating_sub(2);
    let body = row(rect, 1, body_height);
    let (lines, skip) = document_lines(editor, body_height, focused, ctx);
    frame.render_widget(Paragraph::new(lines), body);
    if rect.height > 1 {
        let last = row(rect, rect.height - 1, 1);
        frame.render_widget(Paragraph::new(diagnostics_line(editor, ctx)), last);
    }
    if let Some(popup) = editor.popup.as_ref().filter(|_| focused) {
        let cursor = editor.document.cursor();
        let at = u16::try_from(cursor.row.saturating_sub(skip) + 1).unwrap_or(u16::MAX);
        let col = u16::try_from(cursor.col + 1).unwrap_or(u16::MAX);
        draw_popup(frame, body, at, col, popup, ctx);
    }
}

/// Where the schema comes from, or why there is none.
pub fn schema_line(editor: &GraphqlEditor, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let dot = ctx.ui.glyphs.dot;
    let (text, token) = match &editor.schema {
        SchemaState::Unloaded | SchemaState::Loading => (
            format!("Schema: loading{}", ctx.ui.glyphs.ellipsis),
            Token::Info,
        ),
        SchemaState::Ready(Some(loaded)) => (
            format!(
                "Schema: {} {dot} {} types",
                loaded.origin,
                loaded.schema.type_count()
            ),
            Token::Muted,
        ),
        SchemaState::Ready(None) => (
            "No schema: syntax only. Refresh the schema to validate.".to_owned(),
            Token::Warn,
        ),
        SchemaState::Failed(reason) => (format!("No schema: {reason}"), Token::Warn),
    };
    let mut spans = vec![Span::styled(format!(" {text}"), theme.fg(token))];
    if let Some(file) = editor.document_file() {
        spans.push(Span::styled(
            format!(" {dot} from {file} (read-only)"),
            theme.fg(Token::Muted),
        ));
    }
    Line::from(spans)
}

/// The first error (or warning) and how many more there are.
pub fn diagnostics_line(editor: &GraphqlEditor, ctx: &RenderCtx) -> Line<'static> {
    let ui = ctx.ui;
    if let Some(error) = &editor.variables_error {
        let text = format!(" {} {error}", ui.glyphs.fail);
        return Line::styled(text, ui.theme.fg(Token::Error));
    }
    let validation = &editor.validation;
    let Some(first) = shown_diagnostic(editor) else {
        let text = if validation.syntax_only {
            "Syntax OK (no schema)"
        } else {
            "Valid"
        };
        return Line::styled(format!(" {} {text}", ui.glyphs.ok), ui.theme.fg(Token::Ok));
    };
    let (glyph, token) = match first.severity {
        Severity::Error => (ui.glyphs.fail, Token::Error),
        Severity::Warning => (ui.glyphs.warn, Token::Warn),
    };
    let more = validation.diagnostics.len() - 1;
    let more = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    Line::styled(format!(" {glyph} {first}{more}"), ui.theme.fg(token))
}

/// The diagnostic the line shows: of the errors (else the warnings), the
/// last one at or before the cursor on its line, else the first.
fn shown_diagnostic(editor: &GraphqlEditor) -> Option<&Diagnostic> {
    let validation = &editor.validation;
    let errors: Vec<&Diagnostic> = validation.errors().collect();
    let pool = if errors.is_empty() {
        validation.warnings().collect()
    } else {
        errors
    };
    let cursor = editor.document.cursor();
    let near = pool
        .iter()
        .filter_map(|d| d.position.map(|p| (p, *d)))
        .filter(|(p, _)| p.line == cursor.row + 1 && p.column <= cursor.col + 1)
        .max_by_key(|(p, _)| p.column)
        .map(|(_, d)| d);
    near.or_else(|| pool.first().copied())
}

/// The document rows on screen and how many rows are scrolled off.
fn document_lines(
    editor: &GraphqlEditor,
    height: u16,
    focused: bool,
    ctx: &RenderCtx,
) -> (Vec<Line<'static>>, usize) {
    let area = &editor.document;
    if area.is_empty() && !focused {
        return (
            vec![muted("Type a query, e.g. { user(id: 1) { name } }", ctx)],
            0,
        );
    }
    let mut styles = document_styles(editor, ctx);
    let cursor = area.cursor();
    if focused && let Some(line) = styles.get_mut(cursor.row) {
        if line.len() <= cursor.col {
            line.resize(cursor.col + 1, ctx.ui.theme.fg(Token::Text));
        }
        line[cursor.col] = line[cursor.col].add_modifier(Modifier::REVERSED);
    }
    let rows = usize::from(height).max(1);
    let skip = (cursor.row + 1).saturating_sub(rows);
    let lines = area
        .lines()
        .iter()
        .zip(styles)
        .skip(skip)
        .take(rows)
        .map(|(text, styles)| {
            let mut chars: Vec<char> = text.chars().collect();
            chars.resize(styles.len(), ' ');
            let mut line = group(&chars, &styles);
            line.spans.insert(0, Span::raw(" "));
            line
        })
        .collect();
    (lines, skip)
}

/// The style of every character of every document line: syntax colours,
/// then diagnostics underlined.
fn document_styles(editor: &GraphqlEditor, ctx: &RenderCtx) -> Vec<Vec<Style>> {
    let text = editor.document.text();
    let base = ctx.ui.theme.fg(Token::Text);
    let mut styles: Vec<Vec<Style>> = editor
        .document
        .lines()
        .iter()
        .map(|l| vec![base; l.chars().count()])
        .collect();
    let at = char_positions(&text);
    for h in highlight(&text) {
        let style = class_style(h.class, ctx);
        for &(row, col) in &at[h.start..h.end] {
            if let Some(cell) = styles.get_mut(row).and_then(|l| l.get_mut(col)) {
                *cell = style;
            }
        }
    }
    underline_diagnostics(editor, &mut styles, ctx);
    styles
}

/// The (row, column) of the character at every byte offset of `text`.
fn char_positions(text: &str) -> Vec<(usize, usize)> {
    let mut at = Vec::with_capacity(text.len() + 1);
    let (mut row, mut col) = (0, 0);
    for c in text.chars() {
        at.extend(std::iter::repeat_n((row, col), c.len_utf8()));
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    at.push((row, col));
    at
}

fn class_style(class: TokenClass, ctx: &RenderCtx) -> Style {
    let theme = &ctx.ui.theme;
    match class {
        TokenClass::Keyword => theme.fg(Token::Accent).add_modifier(Modifier::BOLD),
        TokenClass::Name => theme.fg(Token::Text),
        TokenClass::Variable => theme.fg(Token::Info),
        TokenClass::Directive => theme.fg(Token::Warn),
        TokenClass::String | TokenClass::Number => theme.fg(Token::Ok),
        TokenClass::Comment => theme.fg(Token::Muted).add_modifier(Modifier::ITALIC),
        TokenClass::Punctuation => theme.fg(Token::Muted),
    }
}

/// Underline the name each diagnostic points at (one character when it
/// points at no name).
fn underline_diagnostics(editor: &GraphqlEditor, styles: &mut [Vec<Style>], ctx: &RenderCtx) {
    let lines = editor.document.lines();
    for diagnostic in &editor.validation.diagnostics {
        let Some(at) = diagnostic.position else {
            continue;
        };
        let (row, col) = (at.line.saturating_sub(1), at.column.saturating_sub(1));
        let Some(line) = lines.get(row) else { continue };
        let width = line
            .chars()
            .skip(col)
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count()
            .max(1);
        let token = match diagnostic.severity {
            Severity::Error => Token::Error,
            Severity::Warning => Token::Warn,
        };
        let mark = ctx.ui.theme.fg(token).add_modifier(Modifier::UNDERLINED);
        let cells = &mut styles[row];
        let end = (col + width).min(cells.len());
        for cell in cells.iter_mut().take(end).skip(col) {
            *cell = mark;
        }
    }
}

/// Consecutive characters with the same style become one span.
fn group(chars: &[char], styles: &[Style]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut current = styles.first().copied().unwrap_or_default();
    for (c, style) in chars.iter().zip(styles) {
        if *style != current && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), current));
        }
        current = *style;
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, current));
    }
    Line::from(spans)
}

/// The completion popup under the cursor, `at` rows and `col` columns into
/// `body`.
fn draw_popup(frame: &mut Frame, body: Rect, at: u16, col: u16, popup: &Popup, ctx: &RenderCtx) {
    let theme = &ctx.ui.theme;
    let skip = (popup.selected + 1).saturating_sub(POPUP_ROWS);
    let shown: Vec<(usize, String)> = popup
        .items
        .iter()
        .enumerate()
        .skip(skip)
        .take(POPUP_ROWS)
        .map(|(i, c)| (i, popup_text(c)))
        .collect();
    let widest = shown
        .iter()
        .map(|(_, t)| t.chars().count())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(widest).unwrap_or(u16::MAX).min(body.width);
    let x = body.x + col.min(body.width.saturating_sub(width));
    let height = u16::try_from(shown.len()).unwrap_or(0);
    let rows_below = body.height.saturating_sub(at);
    let y = if rows_below >= height {
        body.y + at
    } else {
        body.y + at.saturating_sub(height + 1)
    };
    let rect = Rect::new(x, y, width, height).intersection(body);
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

/// ` posts: [Post!]! `: the signature, which names the candidate, or the
/// name then its type.
fn popup_text(candidate: &rustman_graphql::Completion) -> String {
    if candidate.detail.starts_with(&candidate.label) {
        format!(" {} ", candidate.detail)
    } else {
        format!(" {}  {} ", candidate.label, candidate.detail)
    }
}

// ---- Variables ---------------------------------------------------------

fn variables(
    frame: &mut Frame,
    rect: Rect,
    editor: &GraphqlEditor,
    focused: bool,
    ctx: &RenderCtx,
) {
    let height = rect.height.saturating_sub(1);
    let lines = if editor.variables.is_empty() && !focused {
        vec![muted(
            "No variables. Type a JSON object, e.g. {\"id\": \"{{user_id}}\"}",
            ctx,
        )]
    } else {
        crate::http_view::text(&editor.variables, height, focused, ctx)
    };
    frame.render_widget(Paragraph::new(lines), row(rect, 0, height));
    if rect.height > 1 {
        let last = row(rect, rect.height - 1, 1);
        frame.render_widget(Paragraph::new(diagnostics_line(editor, ctx)), last);
    }
}

// ---- Schema explorer ------------------------------------------------------

/// A row of an open type: a field, argument, enum value, member...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub text: String,
    /// The named type `Enter` opens.
    pub target: Option<String>,
    pub deprecated: bool,
}

/// `[User!]!` names `User`.
fn named(ty: &str) -> String {
    ty.trim_matches(|c| matches!(c, '[' | ']' | '!')).to_owned()
}

fn deprecation(reason: Option<&String>) -> String {
    reason.map_or_else(String::new, |r| format!("  (deprecated: {r})"))
}

/// The rows of an open type, in the order the explorer shows them.
pub fn member_rows(detail: &TypeDetail) -> Vec<MemberRow> {
    let mut rows: Vec<MemberRow> = Vec::new();
    rows.extend(detail.fields.iter().map(|f| MemberRow {
        text: format!("{}{}", f.signature(), deprecation(f.deprecated.as_ref())),
        target: Some(named(&f.ty)),
        deprecated: f.deprecated.is_some(),
    }));
    rows.extend(detail.input_fields.iter().map(|f| MemberRow {
        text: format!("{}{}", f.signature(), deprecation(f.deprecated.as_ref())),
        target: Some(named(&f.ty)),
        deprecated: f.deprecated.is_some(),
    }));
    rows.extend(detail.enum_values.iter().map(|v| MemberRow {
        text: format!("{}{}", v.name, deprecation(v.deprecated.as_ref())),
        target: None,
        deprecated: v.deprecated.is_some(),
    }));
    let linked = |label: &str, name: &String| MemberRow {
        text: format!("{label} {name}"),
        target: Some(name.clone()),
        deprecated: false,
    };
    rows.extend(detail.interfaces.iter().map(|n| linked("implements", n)));
    rows.extend(
        detail
            .possible_types
            .iter()
            .map(|n| linked("possible type", n)),
    );
    rows
}

pub fn kind_label(kind: TypeKind) -> &'static str {
    match kind {
        TypeKind::Scalar => "scalar",
        TypeKind::Object => "type",
        TypeKind::Interface => "interface",
        TypeKind::Union => "union",
        TypeKind::Enum => "enum",
        TypeKind::InputObject => "input",
    }
}

fn explorer(
    editor: &GraphqlEditor,
    height: u16,
    focused: bool,
    ctx: &RenderCtx,
) -> Vec<Line<'static>> {
    let Some(loaded) = editor.loaded() else {
        let mut lines = vec![schema_line(editor, ctx)];
        lines.push(muted(
            "Refresh the schema, or set schema_file, to browse it.",
            ctx,
        ));
        return lines;
    };
    let rows = usize::from(height.saturating_sub(1)).max(1);
    match &editor.explorer.open {
        Some((detail, selected)) => {
            let mut lines = vec![type_heading(detail, ctx)];
            let members = member_rows(detail);
            lines.extend(list(&members, *selected, rows, focused, ctx));
            lines
        }
        None => {
            let overview = &loaded.overview;
            let roots: Vec<String> = [
                ("query", &overview.query),
                ("mutation", &overview.mutation),
                ("subscription", &overview.subscription),
            ]
            .into_iter()
            .filter_map(|(label, name)| name.as_ref().map(|n| format!("{label}: {n}")))
            .collect();
            let heading = format!(
                "{} types {} {}",
                overview.types.len(),
                ctx.ui.glyphs.dot,
                roots.join("  ")
            );
            let mut lines = vec![muted(&heading, ctx)];
            let types: Vec<MemberRow> = overview
                .types
                .iter()
                .map(|t| MemberRow {
                    text: format!("{:<9} {}", kind_label(t.kind), t.name),
                    target: Some(t.name.clone()),
                    deprecated: false,
                })
                .collect();
            lines.extend(list(&types, editor.explorer.selected, rows, focused, ctx));
            lines
        }
    }
}

fn type_heading(detail: &TypeDetail, ctx: &RenderCtx) -> Line<'static> {
    let theme = &ctx.ui.theme;
    let mut spans = vec![
        Span::styled(
            format!(" {} {}", kind_label(detail.kind), detail.name),
            theme.fg(Token::Accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Backspace back".to_owned(), theme.fg(Token::Muted)),
    ];
    if let Some(description) = &detail.description {
        let first = description.lines().next().unwrap_or_default();
        spans.push(Span::styled(
            format!("  {} {first}", ctx.ui.glyphs.dot),
            theme.fg(Token::Muted),
        ));
    }
    Line::from(spans)
}

fn list(
    rows: &[MemberRow],
    selected: usize,
    height: usize,
    focused: bool,
    ctx: &RenderCtx,
) -> Vec<Line<'static>> {
    let theme = &ctx.ui.theme;
    if rows.is_empty() {
        return vec![muted("Nothing to show.", ctx)];
    }
    let skip = (selected + 1).saturating_sub(height);
    rows.iter()
        .enumerate()
        .skip(skip)
        .take(height)
        .map(|(i, r)| {
            let pointer = if i == selected {
                ctx.ui.glyphs.pointer
            } else {
                " "
            };
            let mut style = if r.deprecated {
                theme.fg(Token::Warn).add_modifier(Modifier::CROSSED_OUT)
            } else {
                theme.fg(Token::Text)
            };
            if i == selected && focused {
                style = theme.selection();
            }
            Line::styled(format!("{pointer}{}", r.text), style)
        })
        .collect()
}
