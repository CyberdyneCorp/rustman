//! The GraphQL result in the response viewer (spec: TUI GraphQL pane): the
//! `Data`, `Errors`, `Extensions` and `Events` tabs, prepared off the render
//! loop from the redacted body and the redacted GraphQL record.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::run::GraphqlRecord;
use rustman_graphql::{GraphqlResponse, ResponseError};
use serde_json::Value;

use crate::config::UiOptions;
use crate::tabs::LiveEvent;
use crate::theme::Token;
use crate::view::hint;

/// One error of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRow {
    pub message: String,
    /// `path: user.email · at 1:3 · code: FORBIDDEN`, what of it is there.
    pub details: String,
}

/// One subscription event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub elapsed_ms: u64,
    /// The payload on one line.
    pub payload: String,
}

/// A GraphQL exchange as the viewer shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphqlView {
    /// `query`, `mutation` or `subscription`, with the operation's name.
    pub operation: String,
    /// `ok`, `partial` or `errors`; `None` for a non-GraphQL body.
    pub result: Option<String>,
    pub data: Vec<String>,
    pub errors: Vec<ErrorRow>,
    pub extensions: Vec<String>,
    pub events: Vec<EventRow>,
    /// Why a subscription ended, and how many events it got in all.
    pub ended: Option<(String, usize)>,
    pub apq: Option<String>,
    /// The subscription protocol: `graphql-transport-ws`, `legacy
    /// subscriptions-transport-ws` or `sse`.
    pub protocol: Option<String>,
}

impl GraphqlView {
    /// The view of `record`, with `body` (redacted) when it is in memory.
    pub fn new(record: &GraphqlRecord, body: Option<&[u8]>) -> Self {
        let parsed = body.and_then(GraphqlResponse::parse);
        let operation = match &record.operation_name {
            Some(name) => format!("{} {name}", record.operation_type),
            None => record.operation_type.clone(),
        };
        let mut view = Self {
            operation,
            result: record.result.clone(),
            apq: record.apq.clone(),
            protocol: record.protocol.clone(),
            ..Self::default()
        };
        if let Some(parsed) = parsed {
            view.data = pretty(parsed.data.as_ref());
            view.errors = parsed.errors.iter().map(error_row).collect();
            view.extensions = pretty(parsed.extensions.map(Value::Object).as_ref());
        }
        if let Some(sub) = &record.subscription {
            view.events = sub
                .events
                .iter()
                .map(|e| event_row(e.elapsed_ms, &e.payload))
                .collect();
            view.ended = Some((sub.ended.clone(), sub.event_count));
        }
        view
    }

    /// `subscription OnMessage · ok · legacy subscriptions-transport-ws`:
    /// the operation, the result, the protocol and the APQ outcome, what of
    /// them there is.
    pub fn summary(&self) -> String {
        let parts = [&self.result, &self.protocol, &self.apq];
        std::iter::once(&self.operation)
            .chain(parts.into_iter().flatten())
            .filter(|p| !p.is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn pretty(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| serde_json::to_string_pretty(v).ok())
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn error_row(error: &ResponseError) -> ErrorRow {
    let mut details = Vec::new();
    let path = error.path_text();
    if !path.is_empty() {
        details.push(format!("path: {path}"));
    }
    if let Some(at) = error.locations.first() {
        details.push(format!("at {}:{}", at.line, at.column));
    }
    if let Some(code) = error.code() {
        details.push(format!("code: {code}"));
    }
    ErrorRow {
        message: error.message.clone(),
        details: details.join(" · "),
    }
}

/// An event as a row; `elapsed_ms` is rounded.
pub fn event_row(elapsed_ms: f64, payload: &Value) -> EventRow {
    EventRow {
        elapsed_ms: elapsed_ms.max(0.0).round() as u64,
        payload: payload.to_string(),
    }
}

/// The lines of the GraphQL tab `label` (`Data`, `Errors`...), under the
/// view's summary.
pub fn lines(view: &GraphqlView, label: &str, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(view.summary(), ui.theme.fg(Token::Muted))];
    lines.extend(match label {
        "Data" => data_lines(view, ui),
        "Errors" => error_lines(&view.errors, ui),
        "Extensions" => json_lines(&view.extensions, "No extensions.", ui),
        _ => event_lines(&view.events, 1, view.ended.as_ref(), ui),
    });
    lines
}

fn data_lines(view: &GraphqlView, ui: &UiOptions) -> Vec<Line<'static>> {
    if view.result.is_none() {
        return vec![hint(
            "The response is not a GraphQL result: see the Body tab.",
            ui,
        )];
    }
    json_lines(&view.data, "No data.", ui)
}

fn json_lines(lines: &[String], empty: &str, ui: &UiOptions) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![hint(empty, ui)];
    }
    lines
        .iter()
        .map(|l| Line::styled(l.clone(), ui.theme.fg(Token::Text)))
        .collect()
}

fn error_lines(errors: &[ErrorRow], ui: &UiOptions) -> Vec<Line<'static>> {
    if errors.is_empty() {
        return vec![hint("No errors.", ui)];
    }
    let mut lines = Vec::new();
    for error in errors {
        lines.push(Line::styled(
            format!("{} {}", ui.glyphs.fail, error.message),
            ui.theme.fg(Token::Error).add_modifier(Modifier::BOLD),
        ));
        if !error.details.is_empty() {
            lines.push(Line::styled(
                format!("  {}", error.details),
                ui.theme.fg(Token::Muted),
            ));
        }
    }
    lines
}

/// The events, numbered from `first`, then why the subscription ended.
pub fn event_lines(
    events: &[EventRow],
    first: usize,
    ended: Option<&(String, usize)>,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    if events.is_empty() && ended.is_none() {
        return vec![hint("No events: this is not a subscription.", ui)];
    }
    let mut lines: Vec<Line<'static>> = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            Line::from(vec![
                Span::styled(
                    format!("#{:<3} +{} ms ", first + i, e.elapsed_ms),
                    ui.theme.fg(Token::Accent),
                ),
                Span::styled(e.payload.clone(), ui.theme.fg(Token::Text)),
            ])
        })
        .collect();
    if let Some((reason, count)) = ended {
        let kept = if *count > events.len() {
            format!(" ({} of {count} kept)", events.len())
        } else {
            String::new()
        };
        lines.push(hint(&format!("Ended: {reason}, {count} events{kept}"), ui));
    }
    lines
}

/// The rows of a subscription still running.
pub fn live_rows(events: &[LiveEvent]) -> Vec<EventRow> {
    events
        .iter()
        .map(|e| event_row(e.elapsed_ms, &e.payload))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::run::{EventRecord, SubscriptionRecord};
    use serde_json::json;

    fn record() -> GraphqlRecord {
        GraphqlRecord {
            operation_name: Some("Me".into()),
            operation_type: "query".into(),
            transport: "http".into(),
            protocol: None,
            variables: json!({}),
            result: Some("partial".into()),
            apq: None,
            subscription: None,
        }
    }

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn a_partial_result_splits_into_data_errors_and_extensions() {
        let body = json!({
            "data": {"me": {"name": "Ada", "email": null}},
            "errors": [{"message": "private", "path": ["me", "email"],
                "locations": [{"line": 1, "column": 12}], "extensions": {"code": "FORBIDDEN"}}],
            "extensions": {"cost": 3}
        });
        let view = GraphqlView::new(&record(), Some(body.to_string().as_bytes()));
        assert_eq!(view.operation, "query Me");
        let ui = UiOptions::default();
        assert!(text(&lines(&view, "Data", &ui)).contains("\"name\": \"Ada\""));
        assert_eq!(
            text(&lines(&view, "Errors", &ui)),
            "query Me · partial\n✖ private\n  path: me.email · at 1:12 · code: FORBIDDEN\n"
        );
        assert!(text(&lines(&view, "Extensions", &ui)).contains("\"cost\": 3"));
        assert_eq!(
            text(&lines(&view, "Events", &ui)),
            "query Me · partial\nNo events: this is not a subscription.\n"
        );
    }

    #[test]
    fn a_non_graphql_body_points_at_the_body_tab() {
        let mut record = record();
        record.result = None;
        let view = GraphqlView::new(&record, Some(b"<html>502</html>"));
        let ui = UiOptions::default();
        assert!(text(&lines(&view, "Data", &ui)).contains("not a GraphQL result"));
        assert_eq!(text(&lines(&view, "Errors", &ui)), "query Me\nNo errors.\n");
    }

    #[test]
    fn live_rows_keep_their_numbers_when_older_events_are_dropped() {
        let events = [LiveEvent {
            elapsed_ms: 5.0,
            payload: json!({"data": {"n": 301}}),
        }];
        let lines = event_lines(&live_rows(&events), 301, None, &UiOptions::default());
        assert_eq!(text(&lines), "#301 +5 ms {\"data\":{\"n\":301}}\n");
    }

    #[test]
    fn subscription_events_are_listed_with_why_they_ended() {
        let mut record = record();
        record.operation_type = "subscription".into();
        let events = (1..=3)
            .map(|n| EventRecord {
                ts: String::new(),
                elapsed_ms: f64::from(n) * 10.4,
                payload: json!({"data": {"n": n}}),
            })
            .collect();
        record.subscription = Some(SubscriptionRecord::new(events, "stopped"));
        let view = GraphqlView::new(&record, None);
        let shown = text(&lines(&view, "Events", &UiOptions::default()));
        assert_eq!(
            shown,
            "subscription Me · partial\n\
             #1   +10 ms {\"data\":{\"n\":1}}\n#2   +21 ms {\"data\":{\"n\":2}}\n\
             #3   +31 ms {\"data\":{\"n\":3}}\nEnded: stopped, 3 events\n"
        );
    }

    #[test]
    fn the_summary_shows_the_legacy_protocol_and_the_apq_outcome() {
        let mut record = record();
        record.operation_type = "subscription".into();
        record.operation_name = Some("OnMessage".into());
        record.result = Some("ok".into());
        record.protocol = Some("legacy subscriptions-transport-ws".into());
        let view = GraphqlView::new(&record, None);
        let shown = text(&lines(&view, "Events", &UiOptions::default()));
        assert!(
            shown.starts_with("subscription OnMessage · ok · legacy subscriptions-transport-ws\n"),
            "{shown}"
        );
        let mut query = self::record();
        query.apq = Some("APQ hit".into());
        let view = GraphqlView::new(&query, None);
        assert_eq!(view.summary(), "query Me · partial · APQ hit");
    }
}
