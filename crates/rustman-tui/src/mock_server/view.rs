//! Drawing the mock server pane: the server status line, the live request
//! log with each response's status and source, and the key hints.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use rustman_mock::{LogEntry, Source};

use super::{LogRow, MockPane, Server};
use crate::app::State;
use crate::config::UiOptions;
use crate::theme::Token;
use crate::view::block;

/// Draw the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let ui = &state.ui;
    let pane = &state.mock;
    frame.render_widget(Clear, area);
    let outer = block(" Mock server ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [status, log, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(status_line(pane, ui)), status);
    render_log(frame, log, pane, ui);
    frame.render_widget(Paragraph::new(hint_line(pane, ui)), hints);
}

fn status_line(pane: &MockPane, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    let (text, token) = match &pane.server {
        Server::Stopped => ("Stopped".to_owned(), Token::Muted),
        Server::Preparing | Server::Starting => {
            (format!("Starting{}", ui.glyphs.ellipsis), Token::Info)
        }
        Server::Confirming(_) => ("Waiting for confirmation".to_owned(), Token::Warn),
        Server::Running(running) => {
            let remote = !running.addr.ip().is_loopback();
            let text = format!(
                "Serving {} on http://{}{}",
                pane.document,
                running.addr,
                if remote { " (ON NETWORK)" } else { "" }
            );
            (text, if remote { Token::Warn } else { Token::Ok })
        }
        Server::Stopping => (format!("Stopping{}", ui.glyphs.ellipsis), Token::Info),
        Server::Failed(message) => (format!("Failed: {message}"), Token::Error),
    };
    Line::from(Span::styled(text, theme.fg(token)))
}

fn render_log(frame: &mut Frame, rect: Rect, pane: &MockPane, ui: &UiOptions) {
    let block = block(" Requests ", ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = usize::from(inner.height);
    let skip = pane.log.len().saturating_sub(rows);
    let lines: Vec<Line> = pane
        .log
        .iter()
        .skip(skip)
        .map(|row| match row {
            LogRow::Request(entry) => request_line(entry, ui),
            LogRow::Notice(text) => Line::styled(text.clone(), ui.theme.fg(Token::Muted)),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `10:15:30 GET    200 example     /pets/1 getPet`.
fn request_line(entry: &LogEntry, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    let time = entry.time.get(11..19).unwrap_or(&entry.time).to_owned();
    let status = entry
        .status
        .map_or_else(|| "---".to_owned(), |s| s.to_string());
    let status_token = match entry.status {
        Some(200..=399) => Token::Ok,
        Some(400..=499) => Token::Warn,
        _ => Token::Error,
    };
    let matched = entry
        .override_name
        .clone()
        .or_else(|| entry.operation.clone())
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{time} "), theme.fg(Token::Muted)),
        Span::styled(format!("{:<7}", entry.method), theme.fg(Token::Accent)),
        Span::styled(format!("{status:<4}"), theme.fg(status_token)),
        Span::styled(
            format!("{:<12}", entry.source.as_str()),
            theme.fg(source_token(entry.source)),
        ),
        Span::styled(format!("{} ", entry.path), theme.fg(Token::Text)),
        Span::styled(matched, theme.fg(Token::Muted)),
    ];
    if let Some(first) = entry.errors.first() {
        spans.push(Span::styled(format!("  {first}"), theme.fg(Token::Error)));
    }
    Line::from(spans)
}

fn source_token(source: Source) -> Token {
    match source {
        Source::Error => Token::Error,
        Source::Override | Source::Prefer => Token::Accent,
        Source::Proxy => Token::Warn,
        _ => Token::Info,
    }
}

fn hint_line(pane: &MockPane, ui: &UiOptions) -> Line<'static> {
    let d = ui.glyphs.dot;
    let start = match pane.server {
        Server::Running(_) => "stop",
        _ => "start",
    };
    Line::styled(
        format!("s {start} {d} c clear {d} Esc close"),
        ui.theme.fg(Token::Muted),
    )
}
