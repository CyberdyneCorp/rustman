//! Drawing the webhook pane: the listener status line, the capture list
//! with signature badges, the selected capture (headers and body), the
//! event log, the mock editor and the key hints.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph, Wrap};
use rustman_core::Redactor;
use rustman_webhook::listener::is_remote;
use rustman_webhook::{Capture, Signature};

use super::{Listener, MockEditor, WebhookPane, redactor, selected};
use crate::app::State;
use crate::config::UiOptions;
use crate::response::text::size_text;
use crate::theme::Token;
use crate::view::block;

/// Event log rows shown under the captures.
const LOG_ROWS: u16 = 5;

/// Draw the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let ui = &state.ui;
    let pane = &state.webhook;
    frame.render_widget(Clear, area);
    let outer = block(" Webhooks ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [status, body, log, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(LOG_ROWS + 2),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(status_line(pane, ui)), status);
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(body);
    render_list(frame, list, pane, ui);
    match &pane.mocks {
        Some(editor) => render_mocks(frame, detail, editor, ui),
        None => render_detail(frame, detail, pane, ui),
    }
    render_log(frame, log, pane, ui);
    frame.render_widget(Paragraph::new(hint_line(pane, ui)), hints);
}

/// Listener state, address, the network indicator and buffer usage.
fn status_line(pane: &WebhookPane, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    let mut spans = match &pane.listener {
        Listener::Stopped => vec![Span::styled("Stopped", theme.fg(Token::Muted))],
        Listener::Preparing | Listener::Starting => {
            vec![Span::styled(
                format!("Starting{}", ui.glyphs.ellipsis),
                theme.fg(Token::Info),
            )]
        }
        Listener::Confirming(_) => vec![Span::styled(
            "Waiting for confirmation",
            theme.fg(Token::Warn),
        )],
        Listener::Stopping => vec![Span::styled(
            format!("Stopping{}", ui.glyphs.ellipsis),
            theme.fg(Token::Muted),
        )],
        Listener::Failed(message) => vec![Span::styled(
            format!("Not running: {message}"),
            theme.fg(Token::Error),
        )],
        Listener::Running(running) => {
            let mut spans = vec![
                Span::styled(format!("{} ", ui.glyphs.ok), theme.fg(Token::Ok)),
                Span::styled(
                    format!("Listening on http://{}", running.addr),
                    theme.fg(Token::Text).add_modifier(Modifier::BOLD),
                ),
            ];
            if is_remote(running.addr) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    " REACHABLE FROM THE NETWORK ",
                    theme.badge(Token::Error),
                ));
            }
            spans
        }
    };
    let count = pane.captures.len();
    let noun = if count == 1 { "capture" } else { "captures" };
    spans.push(Span::styled(
        format!(
            " {} {count} {noun}, {}",
            ui.glyphs.dot,
            size_text(pane.buffer_bytes as u64)
        ),
        theme.fg(Token::Muted),
    ));
    Line::from(spans)
}

fn render_list(frame: &mut Frame, rect: Rect, pane: &WebhookPane, ui: &UiOptions) {
    let block = block(" Captures ", ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if pane.captures.is_empty() {
        let text = match pane.listener {
            Listener::Running(_) => "Waiting for requests",
            _ => "Press s to start the listener",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(text, ui.theme.fg(Token::Muted))),
            inner,
        );
        return;
    }
    let rows = usize::from(inner.height);
    let first = pane.selected.saturating_sub(rows.saturating_sub(1));
    let redactor = redactor(pane);
    let lines: Vec<Line> = pane
        .captures
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, c)| capture_row(c, i == pane.selected, &redactor, ui))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `10:15:30 POST /hooks/github 1.8 KiB valid`.
fn capture_row(
    capture: &Capture,
    selected: bool,
    redactor: &Redactor,
    ui: &UiOptions,
) -> Line<'static> {
    let theme = &ui.theme;
    let time = capture
        .timestamp()
        .get(11..19)
        .unwrap_or_default()
        .to_owned();
    let text = format!(
        "{time} {} {} {}",
        capture.method,
        redactor.text(&capture.path),
        size_text(capture.body_size() as u64)
    );
    let style = if selected {
        theme.selection().add_modifier(Modifier::BOLD)
    } else {
        theme.fg(Token::Text)
    };
    let mut spans = vec![Span::styled(text, style)];
    if let Some(signature) = capture.signature {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {} ", signature.as_str()),
            theme.badge(signature_token(signature)),
        ));
    }
    Line::from(spans)
}

fn signature_token(signature: Signature) -> Token {
    match signature {
        Signature::Valid => Token::Ok,
        Signature::Missing | Signature::Expired => Token::Warn,
        Signature::Invalid => Token::Error,
    }
}

fn render_detail(frame: &mut Frame, rect: Rect, pane: &WebhookPane, ui: &UiOptions) {
    let Some(capture) = selected(pane) else {
        let title = format!(" Capture {} body: {} ", ui.glyphs.dot, pane.body.label());
        frame.render_widget(block(&title, ui), rect);
        return;
    };
    let redactor = redactor(pane);
    let head = head_lines(capture, &redactor, ui);
    let body = pane.body_cache.lines(capture, pane.body, &redactor);
    let rect_rows = usize::from(rect.height.saturating_sub(2));
    let total = head.len() + body.len();
    let first = pane.scroll.first(total, rect_rows);
    let d = ui.glyphs.dot;
    let mut title = format!(" Capture {d} body: {} ", pane.body.label());
    if total > rect_rows {
        let last = (first + rect_rows).min(total);
        title.push_str(&format!("{d} {}-{last} of {total} ", first + 1));
    }
    let block = block(&title, ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let text_style = ui.theme.fg(Token::Text);
    let end = total.min(first + rect_rows);
    let lines: Vec<Line> = (first..end)
        .map(|row| match head.get(row) {
            Some(line) => line.clone(),
            None => Line::styled(body[row - head.len()].clone(), text_style),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The request line, the receive time, every header in order and the line
/// before the body.
fn head_lines(capture: &Capture, redactor: &Redactor, ui: &UiOptions) -> Vec<Line<'static>> {
    let theme = &ui.theme;
    let summary = format!(
        "#{} {} {} {} from {} -> {}",
        capture.id,
        capture.method,
        redactor.text(&capture.target()),
        capture.version_text(),
        capture.remote,
        capture.status
    );
    let mut lines = vec![
        Line::styled(summary, theme.fg(Token::Text).add_modifier(Modifier::BOLD)),
        Line::styled(capture.timestamp(), theme.fg(Token::Muted)),
    ];
    for header in &capture.headers {
        let value = redactor.header_value(&header.name, &header.value_text());
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", header.name), theme.fg(Token::Accent)),
            Span::styled(value, theme.fg(Token::Text)),
        ]));
    }
    lines.push(Line::default());
    lines
}

fn render_mocks(frame: &mut Frame, rect: Rect, editor: &MockEditor, ui: &UiOptions) {
    let theme = &ui.theme;
    let block = block(" Mocks: webhook/mocks.yaml ", ui).border_style(theme.fg(Token::Focus));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let error_rows = if editor.error.is_some() { 3 } else { 0 };
    let [text_area, error_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(error_rows)]).areas(inner);
    let cursor = editor.text.cursor();
    let rows = usize::from(text_area.height);
    let first = cursor.row.saturating_sub(rows.saturating_sub(1));
    let lines: Vec<Line> = editor
        .text
        .lines()
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, line)| editor_line(line, (i == cursor.row).then_some(cursor.col), theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), text_area);
    if let Some(error) = &editor.error {
        let error = Paragraph::new(Line::styled(error.clone(), theme.fg(Token::Error)))
            .wrap(Wrap { trim: false });
        frame.render_widget(error, error_area);
    }
}

/// A line of the editor, with the cursor drawn reversed at `cursor`.
fn editor_line(line: &str, cursor: Option<usize>, theme: &crate::theme::Theme) -> Line<'static> {
    let style = theme.fg(Token::Text);
    let Some(col) = cursor else {
        return Line::styled(line.to_owned(), style);
    };
    let before: String = line.chars().take(col).collect();
    let at: String = line.chars().nth(col).map_or(" ".into(), String::from);
    let after: String = line.chars().skip(col + 1).collect();
    Line::from(vec![
        Span::styled(before, style),
        Span::styled(at, style.add_modifier(Modifier::REVERSED)),
        Span::styled(after, style),
    ])
}

fn render_log(frame: &mut Frame, rect: Rect, pane: &WebhookPane, ui: &UiOptions) {
    let block = block(" Event log ", ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = usize::from(inner.height);
    let skip = pane.log.len().saturating_sub(rows);
    let style: Style = ui.theme.fg(Token::Text);
    let lines: Vec<Line> = pane
        .log
        .iter()
        .skip(skip)
        .map(|l| Line::styled(l.clone(), style))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn hint_line(pane: &WebhookPane, ui: &UiOptions) -> Line<'static> {
    let d = ui.glyphs.dot;
    let text = if pane.mocks.is_some() {
        format!("Ctrl+S save {d} Esc close without saving")
    } else {
        let start = match pane.listener {
            Listener::Running(_) => "stop",
            _ => "start",
        };
        format!(
            "s {start} {d} {} select {d} v view {d} r replay {d} w save {d} h history {d} y curl {d} c clear {d} m mocks {d} Esc close",
            ui.glyphs.up_down
        )
    };
    Line::styled(text, ui.theme.fg(Token::Muted))
}
