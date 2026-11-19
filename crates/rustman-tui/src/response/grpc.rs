//! The gRPC result in the response viewer (add-grpc-client "TUI gRPC
//! pane"): the `Messages`, `Status` and `Timeline` tabs, prepared off the
//! render loop from the redacted gRPC record. The timeline is filtered by
//! direction (`d`) and by text (`f`).

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_grpc::{GrpcStatus, StatusDetail};
use serde_json::Value;

use crate::config::UiOptions;
use crate::theme::Token;
use crate::view::hint;

/// One timeline entry as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRow {
    /// `sent` or `received`.
    pub direction: String,
    pub at_ms: f64,
    pub size_bytes: u64,
    /// The message as compact JSON.
    pub message: String,
}

/// Which directions the timeline shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectionFilter {
    #[default]
    All,
    Sent,
    Received,
}

impl DirectionFilter {
    /// The next filter of `d`: all, sent, received, all...
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Sent,
            Self::Sent => Self::Received,
            Self::Received => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Sent => "sent",
            Self::Received => "received",
        }
    }

    fn keeps(self, direction: &str) -> bool {
        self == Self::All || self.label() == direction
    }
}

/// The timeline filter of a viewer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TimelineFilter {
    pub direction: DirectionFilter,
    /// Rows whose message contains this text (case-insensitive).
    pub text: String,
}

impl TimelineFilter {
    fn keeps(&self, row: &TimelineRow) -> bool {
        let text = self.text.to_lowercase();
        self.direction.keeps(&row.direction)
            && (text.is_empty() || row.message.to_lowercase().contains(&text))
    }

    fn is_active(&self) -> bool {
        self.direction != DirectionFilter::All || !self.text.is_empty()
    }
}

/// A gRPC exchange as the viewer shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GrpcView {
    /// `server streaming shop.v1.Catalog/ListProducts · OK (0)`.
    pub summary: String,
    /// Each received message, pretty-printed.
    pub messages: Vec<Vec<String>>,
    pub status: Option<GrpcStatus>,
    /// Why the call ended without a status.
    pub error: Option<String>,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    /// Why rustman stopped the stream, when it did.
    pub stopped: Option<String>,
    pub timeline: Vec<TimelineRow>,
}

impl GrpcView {
    pub fn new(record: &Value) -> Self {
        let status = serde_json::from_value::<GrpcStatus>(record["status"].clone()).ok();
        let call_type = record["call_type"].as_str().unwrap_or_default();
        let method = record["method"].as_str().unwrap_or_default();
        let outcome = status
            .as_ref()
            .map_or_else(|| "no status".to_owned(), GrpcStatus::label);
        let summary = format!("{} {method} · {outcome}", call_type.replace('_', " "))
            .trim()
            .to_owned();
        Self {
            summary,
            messages: list(record, "messages").iter().map(pretty).collect(),
            status,
            error: record["error"]["message"].as_str().map(str::to_owned),
            headers: entries(record, "headers"),
            trailers: entries(record, "trailers"),
            stopped: stopped(&record["stopped"]),
            timeline: list(record, "timeline").iter().map(row).collect(),
        }
    }
}

fn list<'a>(record: &'a Value, key: &str) -> &'a [Value] {
    record[key].as_array().map_or(&[], Vec::as_slice)
}

fn entries(record: &Value, key: &str) -> Vec<(String, String)> {
    list(record, key)
        .iter()
        .map(|e| {
            let text = |k: &str| e[k].as_str().unwrap_or_default().to_owned();
            (text("name"), text("value"))
        })
        .collect()
}

fn row(entry: &Value) -> TimelineRow {
    TimelineRow {
        direction: entry["direction"].as_str().unwrap_or_default().to_owned(),
        at_ms: entry["at_ms"].as_f64().unwrap_or_default(),
        size_bytes: entry["size_bytes"].as_u64().unwrap_or_default(),
        message: entry["message"].to_string(),
    }
}

/// `stopped after 100 messages (stream.max_messages)`, from the record.
fn stopped(value: &Value) -> Option<String> {
    let map = value.as_object()?;
    let (kind, n) = map.iter().next()?;
    Some(match kind.as_str() {
        "max_messages" => format!("stopped after {n} messages (stream.max_messages)"),
        _ => format!("stopped after {n} ms (stream.duration_ms)"),
    })
}

fn pretty(value: &Value) -> Vec<String> {
    serde_json::to_string_pretty(value)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The lines of the gRPC tab `label` (`Messages`, `Status`, `Timeline`),
/// under the view's summary.
pub fn lines(
    view: &GrpcView,
    label: &str,
    filter: &TimelineFilter,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        view.summary.clone(),
        ui.theme.fg(Token::Muted),
    )];
    lines.extend(match label {
        "Messages" => message_lines(view, ui),
        "Status" => status_lines(view, ui),
        _ => timeline_lines(view, filter, ui),
    });
    lines
}

fn message_lines(view: &GrpcView, ui: &UiOptions) -> Vec<Line<'static>> {
    if view.messages.is_empty() {
        return vec![hint("No messages: see the Status tab.", ui)];
    }
    let many = view.messages.len() > 1;
    let mut lines = Vec::new();
    for (i, message) in view.messages.iter().enumerate() {
        if many {
            lines.push(Line::styled(
                format!("#{}", i + 1),
                ui.theme.fg(Token::Accent),
            ));
        }
        lines.extend(
            message
                .iter()
                .map(|l| Line::styled(l.clone(), ui.theme.fg(Token::Text))),
        );
    }
    lines
}

fn status_lines(view: &GrpcView, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match (&view.status, &view.error) {
        (Some(status), _) => lines.extend(status_head(status, ui)),
        (None, Some(error)) => lines.push(Line::styled(
            format!("{} {error}", ui.glyphs.fail),
            ui.theme.fg(Token::Error),
        )),
        (None, None) => lines.push(hint("No status.", ui)),
    }
    if let Some(stopped) = &view.stopped {
        lines.push(Line::styled(stopped.clone(), ui.theme.fg(Token::Warn)));
    }
    lines.extend(metadata_lines("Headers", &view.headers, ui));
    lines.extend(metadata_lines("Trailers", &view.trailers, ui));
    lines
}

/// The status line, its message, and one block per detail.
fn status_head(status: &GrpcStatus, ui: &UiOptions) -> Vec<Line<'static>> {
    let ok = status.code == 0;
    let (glyph, token) = if ok {
        (ui.glyphs.ok, Token::Ok)
    } else {
        (ui.glyphs.fail, Token::Error)
    };
    let mut spans = vec![Span::styled(
        format!("{glyph} {}", status.label()),
        ui.theme.fg(token).add_modifier(Modifier::BOLD),
    )];
    if !status.message.is_empty() {
        spans.push(Span::styled(
            format!("  {}", status.message),
            ui.theme.fg(Token::Text),
        ));
    }
    let mut lines = vec![Line::from(spans)];
    if !status.details.is_empty() {
        lines.push(Line::styled("Details", ui.theme.fg(Token::Accent)));
    }
    lines.extend(status.details.iter().flat_map(|d| detail_lines(d, ui)));
    lines
}

fn detail_lines(detail: &StatusDetail, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format!("  {}", detail.type_name()),
        ui.theme.fg(Token::Info),
    )];
    lines.extend(
        detail
            .lines()
            .into_iter()
            .map(|l| Line::styled(format!("    {l}"), ui.theme.fg(Token::Text))),
    );
    lines
}

fn metadata_lines(title: &str, entries: &[(String, String)], ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format!("{title} ({})", entries.len()),
        ui.theme.fg(Token::Accent),
    )];
    lines.extend(entries.iter().map(|(name, value)| {
        Line::from(vec![
            Span::styled(format!("  {name}: "), ui.theme.fg(Token::Muted)),
            Span::styled(value.clone(), ui.theme.fg(Token::Text)),
        ])
    }));
    lines
}

fn timeline_lines(view: &GrpcView, filter: &TimelineFilter, ui: &UiOptions) -> Vec<Line<'static>> {
    if view.timeline.is_empty() {
        return vec![hint("No timeline: this is a unary call.", ui)];
    }
    let shown: Vec<(usize, &TimelineRow)> = view
        .timeline
        .iter()
        .enumerate()
        .filter(|(_, r)| filter.keeps(r))
        .collect();
    let mut lines = vec![filter_line(filter, shown.len(), view.timeline.len(), ui)];
    lines.extend(shown.into_iter().map(|(i, r)| timeline_row(i, r, ui)));
    lines
}

fn filter_line(
    filter: &TimelineFilter,
    shown: usize,
    total: usize,
    ui: &UiOptions,
) -> Line<'static> {
    let text = if filter.is_active() {
        let typed = if filter.text.is_empty() {
            "none"
        } else {
            filter.text.as_str()
        };
        format!(
            "Direction: {} (d) · Filter: {typed} (f) · {shown} of {total}",
            filter.direction.label()
        )
    } else {
        format!("Direction: all (d) · Filter: none (f) · {total} messages")
    };
    Line::styled(text, ui.theme.fg(Token::Muted))
}

/// `#1   > sent          0.5 ms      4 B  {"text":"hi"}`.
pub fn timeline_row(index: usize, row: &TimelineRow, ui: &UiOptions) -> Line<'static> {
    let (arrow, token) = if row.direction == "sent" {
        (">", Token::Info)
    } else {
        ("<", Token::Ok)
    };
    Line::from(vec![
        Span::styled(format!("#{:<3} ", index + 1), ui.theme.fg(Token::Accent)),
        Span::styled(format!("{arrow} {:<9}", row.direction), ui.theme.fg(token)),
        Span::styled(
            format!("{:>8.1} ms {:>6} B  ", row.at_ms, row.size_bytes),
            ui.theme.fg(Token::Muted),
        ),
        Span::styled(row.message.clone(), ui.theme.fg(Token::Text)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn bidi() -> Value {
        json!({
            "protocol": "grpc", "method": "shop.v1.Catalog/Chat", "call_type": "bidi",
            "status": {"code": 0, "name": "OK", "message": "", "details": []},
            "headers": [{"name": "x-served-by", "value": "catalog"}],
            "trailers": [],
            "messages": [{"text": "welcome"}, {"text": "echo: hi"}],
            "timeline": [
                {"direction": "sent", "at_ms": 0.5, "size_bytes": 4, "message": {"text": "hi"}},
                {"direction": "received", "at_ms": 1.0, "size_bytes": 9, "message": {"text": "welcome"}},
                {"direction": "received", "at_ms": 1.5, "size_bytes": 10, "message": {"text": "echo: hi"}}
            ]
        })
    }

    #[test]
    fn the_timeline_filters_by_direction_and_text() {
        let view = GrpcView::new(&bidi());
        let ui = UiOptions::default();
        assert_eq!(view.summary, "bidi shop.v1.Catalog/Chat · OK (0)");
        let all = text(&lines(&view, "Timeline", &TimelineFilter::default(), &ui));
        assert!(all.contains("3 messages"), "{all}");
        let mut filter = TimelineFilter {
            direction: DirectionFilter::All.next(),
            text: String::new(),
        };
        let sent = text(&lines(&view, "Timeline", &filter, &ui));
        assert!(sent.contains("Direction: sent (d)"), "{sent}");
        assert!(
            sent.contains("1 of 3") && !sent.contains("welcome"),
            "{sent}"
        );
        filter.direction = filter.direction.next();
        filter.text = "ECHO".into();
        let echoed = text(&lines(&view, "Timeline", &filter, &ui));
        assert!(echoed.contains("#3") && !echoed.contains("#2 "), "{echoed}");
        assert_eq!(filter.direction.next(), DirectionFilter::All);
    }

    #[test]
    fn status_details_and_metadata_are_listed() {
        let record = json!({
            "method": "shop.v1.Catalog/GetProduct", "call_type": "unary",
            "status": {"code": 3, "name": "INVALID_ARGUMENT", "message": "invalid product id",
                "details": [{"type_url": "type.googleapis.com/google.rpc.BadRequest",
                    "value": {"fieldViolations": [{"field": "id", "description": "must start with p-"}]}}]},
            "headers": [], "trailers": [{"name": "x-trace", "value": "t1"}],
            "messages": [], "timeline": []
        });
        let view = GrpcView::new(&record);
        let ui = UiOptions::default();
        let status = text(&lines(&view, "Status", &TimelineFilter::default(), &ui));
        assert!(
            status.contains("INVALID_ARGUMENT (3)  invalid product id"),
            "{status}"
        );
        assert!(status.contains("google.rpc.BadRequest"), "{status}");
        assert!(status.contains("must start with p-"), "{status}");
        assert!(status.contains("Trailers (1)\n  x-trace: t1"), "{status}");
        let messages = text(&lines(&view, "Messages", &TimelineFilter::default(), &ui));
        assert!(messages.contains("No messages"), "{messages}");
        let timeline = text(&lines(&view, "Timeline", &TimelineFilter::default(), &ui));
        assert!(timeline.contains("unary call"), "{timeline}");
    }

    #[test]
    fn a_call_without_status_shows_why() {
        let record =
            json!({"method": "a.S/M", "error": {"kind": "request", "message": "unreachable"}});
        let view = GrpcView::new(&record);
        assert_eq!(view.summary, "a.S/M · no status");
        let status = text(&lines(
            &view,
            "Status",
            &TimelineFilter::default(),
            &UiOptions::default(),
        ));
        assert!(status.contains("unreachable"), "{status}");
    }
}
