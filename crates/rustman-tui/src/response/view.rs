//! Drawing the response viewer: the summary line, the viewer tabs and their
//! content. Only the rows on screen are built (a virtual list), whatever the
//! body's size.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};
use rustman_core::assertions::Outcome;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};

use super::json_tree::{Kind, Scalar, TreeView};
use super::received::{TREE_LIMIT, page_count};
use super::text::{self, size_text};
use super::timing::{ms_text, sparkline, waterfall};
use super::{BodyKind, BoxKind, Received, ResponseTab, Viewer};
use crate::config::UiOptions;
use crate::focus::Pane;
use crate::keymap::Keymap;
use crate::keymap::actions::ActionId;
use crate::layout::label_rects;
use crate::tabs::{InFlight, Tab};
use crate::theme::Token;
use crate::view::{block, empty_hint, hint, press};

/// What the response pane draws from.
pub struct Ctx<'a> {
    pub ui: &'a UiOptions,
    pub focused: bool,
    pub now: Instant,
    /// The effective keymap, for the keys hints name.
    pub keymap: &'a Keymap,
}

/// The response pane for `tab` (none open: a hint).
pub fn render(frame: &mut Frame, rect: Rect, tab: Option<&Tab>, ctx: &Ctx) {
    let ui = ctx.ui;
    let title = if ctx.focused {
        format!(" {} {} ", ui.glyphs.pointer, Pane::Response.label())
    } else {
        format!(" {} ", Pane::Response.label())
    };
    let mut pane = block(&title, ui).padding(Padding::horizontal(1));
    if ctx.focused {
        pane = pane
            .border_style(ui.theme.fg(Token::Focus))
            .title_style(ui.theme.fg(Token::Focus).add_modifier(Modifier::BOLD));
    }
    let inner = pane.inner(rect);
    frame.render_widget(pane, rect);
    if inner.height == 0 {
        return;
    }
    let summary = tab.map_or_else(
        || hint("No response yet.", ui),
        |tab| summary_line(tab, ctx),
    );
    frame.render_widget(Paragraph::new(summary), row(inner, 0, 1));
    if let Some(flight) = tab.and_then(|t| t.in_flight.as_ref()) {
        live_events(
            frame,
            row(inner, 1, inner.height.saturating_sub(1)),
            flight,
            ui,
        );
        return;
    }
    let Some((tab, received)) = tab.and_then(|t| t.exchange.as_deref().map(|r| (t, r))) else {
        return;
    };
    if tab.in_flight.is_some() || inner.height < 3 {
        return;
    }
    tabs_bar(frame, row(inner, 1, 1), &tab.viewer, ctx);
    let boxed = usize::from(tab.viewer.input.is_some());
    let height = usize::from(inner.height).saturating_sub(2 + boxed);
    let width = usize::from(inner.width);
    let lines = content(received, &tab.viewer, height, width, ctx);
    let rows = u16::try_from(height).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines), row(inner, 2, rows));
    if let Some(input) = &tab.viewer.input {
        let at = row(inner, inner.height - 1, 1);
        frame.render_widget(Paragraph::new(box_line(input, ui)), at);
    }
}

fn row(inner: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(inner.x, inner.y.saturating_add(offset), inner.width, height).intersection(inner)
}

/// The rows of the content area of a response pane of `rect`, for scrolling.
pub fn content_height(rect: Rect, viewer: &Viewer) -> usize {
    let inner = rect.inner(Margin::new(1, 1));
    usize::from(inner.height).saturating_sub(2 + usize::from(viewer.input.is_some()))
}

/// The summary line (spec: Response summary line), or the spinner while
/// the request is in flight.
pub fn summary_line(tab: &Tab, ctx: &Ctx) -> Line<'static> {
    let ui = ctx.ui;
    if let Some(flight) = &tab.in_flight {
        // `Esc` unsubscribes a JSON-RPC subscription (and keeps its events).
        let keys = if tab.editor.jsonrpc().is_some() {
            "Esc unsubscribes"
        } else {
            "Alt+X stops, Esc cancels"
        };
        return sending_line(flight, keys, ctx);
    }
    match tab.exchange.as_deref() {
        None => {
            let send = press(ctx.keymap, ActionId::RequestSend, "send");
            hint(&empty_hint("No response yet", send, ui), ui)
        }
        Some(received) => outcome_line(received, ui),
    }
}

fn sending_line(flight: &InFlight, keys: &str, ctx: &Ctx) -> Line<'static> {
    let elapsed = ctx.now.saturating_duration_since(flight.started);
    let frames = ctx.ui.glyphs.spinner;
    let frame = usize::try_from(elapsed.as_millis() / 100).unwrap_or(0) % frames.len();
    let events = flight.live_count();
    let text = if events > 0 {
        format!(
            "{} Subscribed: {events} events {} {:.1} s  ({keys})",
            frames[frame],
            ctx.ui.glyphs.dot,
            elapsed.as_secs_f64()
        )
    } else {
        format!(
            "{} Sending… {:.1} s  (Esc cancels)",
            frames[frame],
            elapsed.as_secs_f64()
        )
    };
    Line::styled(text, ctx.ui.theme.fg(Token::Info))
}

/// The newest events of a subscription that is still running: one line
/// each, as many as fit.
fn live_events(frame: &mut Frame, rect: Rect, flight: &InFlight, ui: &UiOptions) {
    let (first, events) = flight.newest_live(usize::from(rect.height));
    if events.is_empty() {
        return;
    }
    let rows = super::graphql::live_rows(&events);
    let lines = super::graphql::event_lines(&rows, first, None, ui);
    frame.render_widget(Paragraph::new(lines), rect);
}

fn outcome_line(received: &Received, ui: &UiOptions) -> Line<'static> {
    let record = &received.record;
    if received.is_cancelled() {
        let text = format!("Cancelled after {:.1} s", record.duration_ms / 1000.0);
        return Line::styled(text, ui.theme.fg(Token::Warn));
    }
    match &record.exchange {
        Some(exchange) => {
            let status = exchange.response.status;
            let mut spans = status_spans(status, ui);
            let dot = ui.glyphs.dot;
            let mut rest = format!(
                " {dot} {} {dot} {}",
                ms_text(exchange.timings.total_ms),
                size_text(exchange.response.body_bytes)
            );
            if let Some(tally) = tally(record) {
                rest.push_str(&format!(" {dot} {tally}"));
            }
            spans.push(Span::styled(rest, ui.theme.fg(Token::Text)));
            if exchange.insecure {
                spans.push(Span::styled(
                    format!(" {dot} TLS: INSECURE"),
                    ui.theme.fg(Token::Error).add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        }
        None => failure_line(record, ui),
    }
}

/// `✔ 201 Created`, coloured by status class.
fn status_spans(status: u16, ui: &UiOptions) -> Vec<Span<'static>> {
    let token = match status {
        200..=299 => Token::Ok,
        300..=399 => Token::Accent,
        400..=499 => Token::Warn,
        _ => Token::Error,
    };
    let symbol = if status < 400 {
        ui.glyphs.ok
    } else {
        ui.glyphs.fail
    };
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .map(|r| format!(" {r}"))
        .unwrap_or_default();
    vec![Span::styled(
        format!("{symbol} {status}{reason}"),
        ui.theme.fg(token).add_modifier(Modifier::BOLD),
    )]
}

/// `3/3 passed`; `None` without assertions.
fn tally(record: &RequestRecord) -> Option<String> {
    let total = record.assertions.len();
    let passed = record.assertions.iter().filter(|a| a.passed()).count();
    (total > 0).then(|| format!("{passed}/{total} passed"))
}

/// The error category of a transport failure, from the engine's message.
pub fn error_category(message: &str) -> &'static str {
    const CATEGORIES: [(&str, &str); 7] = [
        ("dns:", "DNS error"),
        ("tls:", "TLS error"),
        ("timeout", "Timeout"),
        ("cannot connect", "Connection error"),
        ("proxy:", "Proxy error"),
        ("oauth2:", "Auth error"),
        ("too many redirects", "Redirect error"),
    ];
    CATEGORIES
        .iter()
        .find(|(prefix, _)| message.starts_with(prefix))
        .map_or("Error", |(_, category)| category)
}

fn failure_line(record: &RequestRecord, ui: &UiOptions) -> Line<'static> {
    let (title, message) = match (record.status, record.skip_reason) {
        (RequestStatus::Skipped, Some(SkipReason::UnsupportedKind)) => {
            ("Not sent", "this build cannot send this request kind")
        }
        _ => {
            let message = record.error.as_deref().unwrap_or("no response");
            (error_category(message), message)
        }
    };
    let message = message.strip_prefix("dns: ").unwrap_or(message);
    let style = ui.theme.fg(Token::Error);
    Line::from(vec![
        Span::styled(
            format!("{} {title}", ui.glyphs.fail),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} {message}", ui.glyphs.dot), style),
    ])
}

fn tabs_bar(frame: &mut Frame, rect: Rect, viewer: &Viewer, ctx: &Ctx) {
    let ui = ctx.ui;
    let tabs = viewer.tabs();
    let labels: Vec<String> = tabs.iter().map(|t| t.label().to_owned()).collect();
    for (tab, at) in tabs.iter().zip(label_rects(rect, &labels)) {
        let style = if *tab == viewer.tab {
            ui.theme
                .fg(Token::Accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            ui.theme.fg(Token::Muted)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!(" {} ", tab.label()), style)),
            at,
        );
    }
}

fn box_line(input: &super::InputBox, ui: &UiOptions) -> Line<'static> {
    let prompt = match input.kind {
        BoxKind::Filter => "JSONPath ",
        BoxKind::Search => "/",
    };
    let mut spans = vec![
        Span::styled(prompt, ui.theme.fg(Token::Accent)),
        Span::styled(format!("{}_", input.text), ui.theme.fg(Token::Text)),
    ];
    if let Some(error) = &input.error {
        spans.push(Span::styled(
            format!("  {} {error}", ui.glyphs.fail),
            ui.theme.fg(Token::Error),
        ));
    }
    Line::from(spans)
}

/// The content lines on screen for the current viewer tab.
pub fn content(
    received: &Received,
    viewer: &Viewer,
    height: usize,
    width: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    match viewer.tab {
        ResponseTab::Body => body(received, viewer, height, ctx),
        tab => {
            let all = tab_lines(received, tab, width, ctx.ui);
            window(all, viewer.scroll, height)
        }
    }
}

/// The lines of a line-based tab, for drawing and scrolling.
pub fn tab_lines(
    received: &Received,
    tab: ResponseTab,
    width: usize,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    match tab {
        ResponseTab::Headers => header_lines(received, ui),
        ResponseTab::Timing => timing_lines(received, width, ui),
        ResponseTab::Assertions => assertion_lines(&received.record, ui),
        ResponseTab::Snapshot => crate::review::lines(&received.snapshots, ui),
        ResponseTab::Body => Vec::new(),
        tab => rpc_lines(received, tab, ui).unwrap_or_else(|| match &received.graphql {
            Some(view) => super::graphql::lines(view, tab.label(), ui),
            None => vec![hint("Not a GraphQL exchange.", ui)],
        }),
    }
}

/// The lines of a JSON-RPC tab; `None` for a GraphQL one.
fn rpc_lines(received: &Received, tab: ResponseTab, ui: &UiOptions) -> Option<Vec<Line<'static>>> {
    match (received.jsonrpc.as_deref(), tab) {
        (Some(view), ResponseTab::Result | ResponseTab::Error | ResponseTab::Events) => {
            Some(super::jsonrpc::lines(view, tab.label(), ui))
        }
        (None, ResponseTab::Result | ResponseTab::Error) => {
            Some(vec![hint("Not a JSON-RPC exchange.", ui)])
        }
        _ => None,
    }
}

fn window(lines: Vec<Line<'static>>, scroll: usize, height: usize) -> Vec<Line<'static>> {
    let start = scroll.min(lines.len().saturating_sub(height));
    lines.into_iter().skip(start).take(height).collect()
}

fn header_lines(received: &Received, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &received.record.exchange {
        Some(exchange) => lines.extend(exchange.response.headers.iter().map(|h| {
            Line::from(vec![
                Span::styled(format!("{}: ", h.name), ui.theme.fg(Token::Accent)),
                Span::styled(h.value.clone(), ui.theme.fg(Token::Text)),
            ])
        })),
        None => lines.push(hint("No response headers.", ui)),
    }
    if !received.captured.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            "Captured variables",
            ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
        ));
        lines.extend(received.captured.iter().map(|c| captured_line(c, ui)));
    }
    lines
}

fn captured_line(captured: &super::Captured, ui: &UiOptions) -> Line<'static> {
    let value = match (&captured.value, captured.secret) {
        (_, true) => ui.glyphs.mask.to_owned(),
        (Some(value), false) => value.clone(),
        (None, false) => captured.message.clone().unwrap_or_default(),
    };
    let token = if captured.value.is_some() || captured.secret {
        Token::Text
    } else {
        Token::Warn
    };
    Line::from(vec![
        Span::styled(format!("{} = ", captured.var), ui.theme.fg(Token::Accent)),
        Span::styled(value, ui.theme.fg(token)),
    ])
}

fn timing_lines(received: &Received, width: usize, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = match &received.record.exchange {
        Some(exchange) => waterfall(&exchange.timings, width, ui),
        None => vec![hint("No timings: the request got no response.", ui)],
    };
    lines.push(Line::default());
    lines.extend(sparkline(&received.history, ui));
    lines
}

fn assertion_lines(record: &RequestRecord, ui: &UiOptions) -> Vec<Line<'static>> {
    if record.assertions.is_empty() {
        return vec![hint("No assertions.", ui)];
    }
    record
        .assertions
        .iter()
        .map(|a| {
            let (symbol, word, token) = match a.outcome {
                Outcome::Pass => (ui.glyphs.ok, "passed", Token::Ok),
                Outcome::Fail => (ui.glyphs.fail, "failed", Token::Error),
                Outcome::Error => (ui.glyphs.warn, "error", Token::Warn),
            };
            let mut spans = vec![
                Span::styled(format!("{symbol} {word} "), ui.theme.fg(token)),
                Span::styled(a.label.clone(), ui.theme.fg(Token::Text)),
            ];
            let dot = ui.glyphs.dot;
            if !a.passed() {
                let expected = a.expected.as_deref().unwrap_or("—");
                let actual = a.actual.as_deref().unwrap_or("—");
                spans.push(Span::styled(
                    format!(" {dot} expected {expected} {dot} actual {actual}"),
                    ui.theme.fg(Token::Muted),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// How many lines the current line-based view has (for scrolling).
pub fn line_count(received: &Received, viewer: &Viewer, width: usize, ui: &UiOptions) -> usize {
    if viewer.tab != ResponseTab::Body {
        return tab_lines(received, viewer.tab, width, ui).len();
    }
    match &received.body {
        BodyKind::Json { pretty, .. } => pretty.len() + 1,
        BodyKind::Text { lines, .. } => lines.len(),
        BodyKind::Binary { head } => head.len().div_ceil(16) + 2,
        BodyKind::Paged => viewer.page.as_ref().map_or(0, |p| p.lines.len()) + 2,
        BodyKind::Empty => 1,
    }
}

fn body(received: &Received, viewer: &Viewer, height: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    let ui = ctx.ui;
    match &received.body {
        BodyKind::Empty => vec![hint("Empty body.", ui)],
        BodyKind::Json { pretty, .. } if viewer.raw => {
            let tree = press(ctx.keymap, ActionId::ResponseRaw, "show the tree");
            let mut lines = vec![hint(&empty_hint("Raw", tree, ui), ui)];
            lines.extend(text_window(
                pretty,
                viewer.scroll,
                height.saturating_sub(1),
                ui,
            ));
            lines
        }
        BodyKind::Json { .. } => tree_lines(viewer, height, ctx),
        BodyKind::Text { lines, styled } => match styled {
            Some(styled) => styled_window(styled, viewer.scroll, height, ui),
            None => text_window(lines, viewer.scroll, height, ui),
        },
        BodyKind::Binary { head } => binary_lines(received.size, head, viewer.scroll, height, ui),
        BodyKind::Paged => paged_lines(received.size, viewer, height, ui),
    }
}

fn text_window(
    lines: &text::TextLines,
    scroll: usize,
    height: usize,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let start = scroll.min(lines.len().saturating_sub(height));
    (start..lines.len().min(start + height))
        .map(|i| Line::styled(lines.line(i).to_owned(), ui.theme.fg(Token::Text)))
        .collect()
}

fn styled_window(
    styled: &[text::StyledLine],
    scroll: usize,
    height: usize,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let start = scroll.min(styled.len().saturating_sub(height));
    styled
        .iter()
        .skip(start)
        .take(height)
        .map(|runs| {
            Line::from(
                runs.iter()
                    .map(|(token, run)| Span::styled(run.clone(), ui.theme.fg(*token)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn binary_lines(
    size: u64,
    head: &[u8],
    scroll: usize,
    height: usize,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let dot = ui.glyphs.dot;
    let shown = if size > head.len() as u64 {
        format!(" {dot} first {} shown", size_text(head.len() as u64))
    } else {
        String::new()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("Binary body {dot} {}{shown}", size_text(size)),
                ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {dot} s Save body…"), ui.theme.fg(Token::Accent)),
        ]),
        Line::default(),
    ];
    let hex = text::hex_lines(head);
    let room = height.saturating_sub(2);
    let start = scroll.min(hex.len().saturating_sub(room));
    lines.extend(
        hex.into_iter()
            .skip(start)
            .take(room)
            .map(|l| Line::styled(l, ui.theme.fg(Token::Muted))),
    );
    lines
}

fn paged_lines(size: u64, viewer: &Viewer, height: usize, ui: &UiOptions) -> Vec<Line<'static>> {
    let limit = size_text(TREE_LIMIT);
    let notice = format!(
        "{} The tree view is disabled above {limit}: this {} body is shown raw, page by page.",
        ui.glyphs.warn,
        size_text(size)
    );
    let mut lines = vec![Line::styled(notice, ui.theme.fg(Token::Warn))];
    let Some(page) = &viewer.page else {
        return lines;
    };
    let status = if page.loading {
        format!(
            "Page {} of {} · loading…",
            page.number + 1,
            page_count(size)
        )
    } else {
        format!(
            "Page {} of {} · PageDown at the end turns the page",
            page.number + 1,
            page_count(size)
        )
    };
    lines.push(hint(&status, ui));
    let room = height.saturating_sub(2);
    let start = viewer.scroll.min(page.lines.len().saturating_sub(room));
    lines.extend(
        page.lines
            .iter()
            .skip(start)
            .take(room)
            .map(|l| Line::styled(l.clone(), ui.theme.fg(Token::Text))),
    );
    lines
}

fn tree_lines(viewer: &Viewer, height: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    let ui = ctx.ui;
    let Some(tree) = viewer.shown_tree() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(filtered) = &viewer.filtered {
        let count = filtered.view.tree.len_top();
        let word = if count == 1 { "match" } else { "matches" };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", filtered.expr), ui.theme.fg(Token::Accent)),
            Span::styled(format!("{count} {word}"), ui.theme.fg(Token::Muted)),
        ]));
    }
    if let Some(search) = viewer.search.as_ref().filter(|s| s.pending) {
        let text = format!(
            "/{} {} searching{}",
            search.query, ui.glyphs.dot, ui.glyphs.ellipsis
        );
        lines.push(hint(&text, ui));
    } else if let Some(search) = &viewer.search {
        let current = tree
            .selected()
            .and_then(|n| search.matches.iter().position(|m| *m == n));
        let at = current.map_or_else(|| "-".to_owned(), |i| (i + 1).to_string());
        lines.push(hint(
            &format!(
                "/{} {} {at} of {} {} n/N next/previous",
                search.query,
                ui.glyphs.dot,
                search.matches.len(),
                ui.glyphs.dot
            ),
            ui,
        ));
    }
    let room = height.saturating_sub(lines.len());
    let matches = viewer.search.as_ref().map(|s| s.matches.as_slice());
    for node in tree.window(room) {
        let selected = ctx.focused && tree.selected() == Some(node);
        let hit = matches.is_some_and(|m| m.binary_search(&node).is_ok());
        lines.push(tree_row(tree, node, selected, hit, ui));
    }
    lines
}

fn scalar_token(kind: &Kind) -> Token {
    match kind {
        Kind::Scalar(Scalar::String, _) => Token::Ok,
        Kind::Scalar(Scalar::Number, _) => Token::Info,
        Kind::Scalar(Scalar::Bool | Scalar::Null, _) => Token::Warn,
        Kind::Object(_) | Kind::Array(_) => Token::Muted,
    }
}

fn tree_row(
    tree: &TreeView,
    index: usize,
    selected: bool,
    hit: bool,
    ui: &UiOptions,
) -> Line<'static> {
    let node = tree.tree.node(index);
    let arrow = match (node.is_container(), tree.is_collapsed(index)) {
        (false, _) => " ",
        (true, true) => ui.glyphs.tree_closed,
        (true, false) => ui.glyphs.tree_open,
    };
    let mut spans = vec![
        Span::raw("  ".repeat(usize::from(node.depth))),
        Span::styled(format!("{arrow} "), ui.theme.fg(Token::Muted)),
    ];
    if let Some(label) = node.label_text() {
        let mut style = ui.theme.fg(Token::Accent);
        if hit {
            style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        }
        spans.push(Span::styled(label, style));
        spans.push(Span::styled(": ", ui.theme.fg(Token::Muted)));
    }
    spans.push(Span::styled(
        node.summary(),
        ui.theme.fg(scalar_token(&node.kind)),
    ));
    let line = Line::from(spans);
    if selected {
        line.style(ui.theme.selection())
    } else {
        line
    }
}

/// The style a test looks for on the summary status.
#[cfg(test)]
pub fn status_style(status: u16, ui: &UiOptions) -> ratatui::style::Style {
    status_spans(status, ui)[0].style
}
