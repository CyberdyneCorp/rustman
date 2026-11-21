//! Drawing the recorder pane: the proxy status line, the filtered capture
//! list with marks, the selected capture (redacted) and the key hints.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph, Wrap};
use rustman_core::Redactor;
use rustman_proxy::capture::{BodyCapture, Header};
use rustman_proxy::{Capture, Exchange, Recorded, TLS_REJECTED_HINT};

use super::{Proxy, RecorderPane, selected, visible};
use crate::app::State;
use crate::config::UiOptions;
use crate::response::text::size_text;
use crate::theme::Token;
use crate::view::block;

/// Body lines shown in the detail view, per body.
const BODY_LINES: usize = 200;

/// Draw the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let ui = &state.ui;
    let pane = &state.recorder;
    frame.render_widget(Clear, area);
    let outer = block(" Traffic recorder ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [status, body, filter, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(status_line(pane, ui)), status);
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    render_list(frame, list, pane, ui);
    render_detail(frame, detail, pane, ui);
    frame.render_widget(Paragraph::new(filter_line(pane, ui)), filter);
    frame.render_widget(Paragraph::new(hint_line(pane, ui)), hints);
}

fn status_line(pane: &RecorderPane, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    let ellipsis = ui.glyphs.ellipsis;
    let mut spans = match &pane.proxy {
        Proxy::Stopped => vec![Span::styled("Stopped", theme.fg(Token::Muted))],
        Proxy::Preparing | Proxy::Starting => {
            vec![Span::styled(
                format!("Starting{ellipsis}"),
                theme.fg(Token::Info),
            )]
        }
        Proxy::Confirming(_) => vec![Span::styled(
            "Waiting for confirmation",
            theme.fg(Token::Warn),
        )],
        Proxy::Stopping => vec![Span::styled(
            format!("Stopping{ellipsis}"),
            theme.fg(Token::Muted),
        )],
        Proxy::Failed(message) => vec![Span::styled(
            format!("Not running: {message}"),
            theme.fg(Token::Error),
        )],
        Proxy::Running(running) => {
            let mut spans = vec![
                Span::styled(format!("{} ", ui.glyphs.ok), theme.fg(Token::Ok)),
                Span::styled(
                    format!("Recording on http://{}", running.addr),
                    theme.fg(Token::Text).add_modifier(Modifier::BOLD),
                ),
            ];
            if !running.addr.ip().is_loopback() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    " REACHABLE FROM THE NETWORK ",
                    theme.badge(Token::Error),
                ));
            }
            if !running.intercept.is_empty() {
                let text = format!("  intercept {}", running.intercept.join(", "));
                spans.push(Span::styled(text, theme.fg(Token::Accent)));
            }
            spans
        }
    };
    let count = pane.captures.len();
    let noun = if count == 1 { "capture" } else { "captures" };
    let marked = pane.marked.len();
    spans.push(Span::styled(
        format!(" {} {count} {noun}, {marked} marked", ui.glyphs.dot),
        theme.fg(Token::Muted),
    ));
    Line::from(spans)
}

fn render_list(frame: &mut Frame, rect: Rect, pane: &RecorderPane, ui: &UiOptions) {
    let block = block(" Captures ", ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let shown = visible(pane);
    if shown.is_empty() {
        let text = match (&pane.proxy, pane.captures.is_empty()) {
            (_, false) => "No capture matches the filter",
            (Proxy::Running(_), true) => "Waiting for traffic",
            _ => "Press s to start the recording proxy",
        };
        let text = Span::styled(text, ui.theme.fg(Token::Muted));
        frame.render_widget(Paragraph::new(text), inner);
        return;
    }
    let rows = usize::from(inner.height);
    let first = pane.selected.saturating_sub(rows.saturating_sub(1));
    let lines: Vec<Line> = shown
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, c)| {
            let marked = pane.marked.contains(&c.id);
            capture_row(c, i == pane.selected, marked, ui)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `* 10:15:30 GET api.test /v1/me 200 12 ms 1.2 KiB`, redacted.
fn capture_row(capture: &Capture, selected: bool, marked: bool, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    let mark = if marked { "*" } else { " " };
    let text = format!(
        "{mark} {} {}",
        clock(capture.started),
        capture.redactor().text(&summary(capture))
    );
    let style = match (selected, &capture.recorded) {
        (true, _) => theme.selection().add_modifier(Modifier::BOLD),
        (false, Recorded::TlsRejected { .. }) => theme.fg(Token::Warn),
        (false, Recorded::Tunnel(_)) => theme.fg(Token::Muted),
        (false, Recorded::Exchange(_)) => theme.fg(Token::Text),
    };
    Line::styled(text, style)
}

/// The list text of a capture (unredacted; the filter matches it):
/// method, host, path, status, duration and size.
pub fn summary(capture: &Capture) -> String {
    match &capture.recorded {
        Recorded::Exchange(e) => {
            let (host, path) = host_and_path(&e.url);
            let status = e.status.map_or_else(|| "---".to_owned(), |s| s.to_string());
            let mut text = format!(
                "{} {host} {path} {status} {} ms {}",
                e.method,
                e.duration.as_millis(),
                size_text(e.response_body.total)
            );
            if e.request_body.truncated || e.response_body.truncated {
                text.push_str(" truncated");
            }
            text
        }
        Recorded::Tunnel(t) => format!(
            "TUNNEL {}:{} {} up, {} down",
            t.host,
            t.port,
            size_text(t.bytes_up),
            size_text(t.bytes_down)
        ),
        Recorded::TlsRejected { host, .. } => format!("TLS REJECTED {host}"),
    }
}

/// `https://api.test:8443/v1/me?q=1` → (`api.test:8443`, `/v1/me?q=1`).
fn host_and_path(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    }
}

/// `HH:MM:SS` (UTC) of `at`.
fn clock(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, day % 3600 / 60, day % 60)
}

fn render_detail(frame: &mut Frame, rect: Rect, pane: &RecorderPane, ui: &UiOptions) {
    let Some(capture) = selected(pane) else {
        frame.render_widget(block(" Capture ", ui), rect);
        return;
    };
    let lines = detail_lines(&capture, ui);
    let rows = usize::from(rect.height.saturating_sub(2));
    let first = pane.scroll.min(lines.len().saturating_sub(rows));
    let mut title = format!(" Capture #{} ", capture.id);
    if lines.len() > rows {
        let last = (first + rows).min(lines.len());
        title.push_str(&format!(
            "{} {}-{last} of {} ",
            ui.glyphs.dot,
            first + 1,
            lines.len()
        ));
    }
    let block = block(&title, ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let shown: Vec<Line> = lines.into_iter().skip(first).take(rows).collect();
    frame.render_widget(Paragraph::new(shown).wrap(Wrap { trim: false }), inner);
}

/// Every line of the detail view, redacted.
pub fn detail_lines(capture: &Capture, ui: &UiOptions) -> Vec<Line<'static>> {
    let theme = &ui.theme;
    let bold = theme.fg(Token::Text).add_modifier(Modifier::BOLD);
    let muted = theme.fg(Token::Muted);
    match &capture.recorded {
        Recorded::Exchange(e) => exchange_lines(e, &capture.redactor(), ui),
        Recorded::Tunnel(t) => {
            let mut lines = vec![
                Line::styled(format!("TUNNEL {}:{}", t.host, t.port), bold),
                Line::styled(
                    format!(
                        "Not decrypted: {} B up, {} B down in {} ms",
                        t.bytes_up,
                        t.bytes_down,
                        t.duration.as_millis()
                    ),
                    muted,
                ),
            ];
            if let Some(error) = &t.error {
                lines.push(Line::styled(error.clone(), theme.fg(Token::Error)));
            }
            lines
        }
        Recorded::TlsRejected { host, port } => vec![
            Line::styled(format!("TLS REJECTED {host}:{port}"), bold),
            Line::styled(TLS_REJECTED_HINT, theme.fg(Token::Warn)),
        ],
    }
}

fn exchange_lines(e: &Exchange, redactor: &Redactor, ui: &UiOptions) -> Vec<Line<'static>> {
    let theme = &ui.theme;
    let bold = theme.fg(Token::Text).add_modifier(Modifier::BOLD);
    let status = e
        .status
        .map_or_else(|| "no response".to_owned(), |s| s.to_string());
    let via = if e.intercepted { ", decrypted" } else { "" };
    let mut lines = vec![
        Line::styled(format!("{} {}", e.method, redactor.text(&e.url)), bold),
        Line::styled(
            format!(
                "{} -> {status} in {} ms{via}",
                e.version,
                e.duration.as_millis()
            ),
            theme.fg(Token::Muted),
        ),
    ];
    if let Some(error) = &e.error {
        let error = redactor.text(error).into_owned();
        lines.push(Line::styled(error, theme.fg(Token::Error)));
    }
    section(
        &mut lines,
        "Request",
        &e.request_headers,
        &e.request_body,
        redactor,
        ui,
    );
    section(
        &mut lines,
        "Response",
        &e.response_headers,
        &e.response_body,
        redactor,
        ui,
    );
    lines
}

fn section(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    headers: &[Header],
    body: &BodyCapture,
    redactor: &Redactor,
    ui: &UiOptions,
) {
    let theme = &ui.theme;
    lines.push(Line::default());
    lines.push(Line::styled(
        title.to_owned(),
        theme.fg(Token::Info).add_modifier(Modifier::BOLD),
    ));
    for (name, value) in headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{name}: "), theme.fg(Token::Accent)),
            Span::styled(redactor.header_value(name, value), theme.fg(Token::Text)),
        ]));
    }
    if body.total == 0 {
        return;
    }
    let mut about = format!("body: {}", size_text(body.total));
    if body.truncated {
        about.push_str(&format!(
            ", stored {} (truncated)",
            size_text(body.bytes.len() as u64)
        ));
    }
    lines.push(Line::styled(about, theme.fg(Token::Muted)));
    let shown = redactor.bytes(&body.bytes);
    match std::str::from_utf8(&shown) {
        Ok(text) => lines.extend(
            text.lines()
                .take(BODY_LINES)
                .map(|l| Line::styled(l.to_owned(), theme.fg(Token::Text))),
        ),
        Err(_) => lines.push(Line::styled("(binary)", theme.fg(Token::Muted))),
    }
}

fn filter_line(pane: &RecorderPane, ui: &UiOptions) -> Line<'static> {
    let theme = &ui.theme;
    if pane.filtering {
        return Line::from(vec![
            Span::styled("Filter: ", theme.fg(Token::Accent)),
            Span::styled(format!("{}_", pane.filter), theme.fg(Token::Text)),
        ]);
    }
    if pane.filter.is_empty() {
        return Line::default();
    }
    let shown = visible(pane).len();
    Line::styled(
        format!("Filter: {} ({shown} shown)", pane.filter),
        theme.fg(Token::Muted),
    )
}

fn hint_line(pane: &RecorderPane, ui: &UiOptions) -> Line<'static> {
    let d = ui.glyphs.dot;
    let text = if pane.filtering {
        format!("Enter keep filter {d} Esc clear filter")
    } else {
        let start = match pane.proxy {
            Proxy::Running(_) => "stop",
            _ => "start",
        };
        format!(
            "s {start} {d} {} select {d} Space mark {d} a mark all {d} / filter {d} w save {d} e export HAR {d} Esc close",
            ui.glyphs.up_down
        )
    };
    Line::styled(text, ui.theme.fg(Token::Muted))
}
