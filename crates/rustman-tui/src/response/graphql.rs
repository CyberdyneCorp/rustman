//! The GraphQL result in the response viewer (spec: TUI GraphQL pane): the
//! `Data`, `Errors`, `Extensions` and `Events` tabs, prepared off the render
//! loop from the redacted body and the redacted GraphQL record. With a
//! blockchain preset (add-evm-toolkit "blockchain-graphql"), `Data` shows
//! the scalars formatted, in token units or raw (`graphql.display`), the
//! summary names the preset and a subgraph's lag, and the addresses and
//! transaction hashes of the result are links into the EVM toolkit.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::Redactor;
use rustman_core::run::GraphqlRecord;
use rustman_evm::gql::{Display, Link, find_links, format_data};
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
    /// The blockchain preset: `preset subgraph`.
    pub preset: Option<String>,
    /// A subgraph's lag: `lag 12 blocks`, `unknown (no rpc)`.
    pub lag: Option<String>,
    /// How `data` is shown, with a preset.
    pub display: Display,
    /// `data` in the units and raw forms (`data` holds the formatted one).
    pub units: Vec<String>,
    pub raw: Vec<String>,
    /// The addresses and transaction hashes of `data`.
    pub links: Vec<Link>,
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
            view.preset_forms(record, parsed.data.as_ref());
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

    /// With a preset: the summary's preset and lag, the display forms of
    /// `data` and its links.
    fn preset_forms(&mut self, record: &GraphqlRecord, data: Option<&Value>) {
        let Some(preset) = &record.preset else {
            return;
        };
        self.preset = Some(format!("preset {}", preset.name));
        self.lag = preset.freshness.as_ref().map(|f| f.lag_text());
        let Some(data) = data else {
            return;
        };
        let form = |display| pretty(Some(&format_data(data, &preset.scalars, display)));
        self.raw = std::mem::replace(&mut self.data, form(Display::Formatted));
        self.units = form(Display::Units);
        self.links = find_links(data);
    }

    /// This view with every display form redacted again (a formatted
    /// value, like a checksummed address, can spell a secret the raw one
    /// did not), and links that would show a secret left out.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let redact = |lines: &mut Vec<String>| {
            for line in lines.iter_mut() {
                *line = redactor.text(line).into_owned();
            }
        };
        redact(&mut self.data);
        redact(&mut self.units);
        self.links.retain(|l| redactor.text(&l.value) == l.value);
        self
    }

    /// The `data` lines as currently shown.
    pub fn shown_data(&self) -> &[String] {
        match self.display {
            Display::Units if self.preset.is_some() => &self.units,
            Display::Raw if self.preset.is_some() => &self.raw,
            _ => &self.data,
        }
    }

    /// `graphql.display`: formatted → units → raw. False without a preset.
    pub fn cycle_display(&mut self) -> bool {
        if self.preset.is_none() {
            return false;
        }
        self.display = self.display.next();
        true
    }

    /// `subscription OnMessage · ok · legacy subscriptions-transport-ws`:
    /// the operation, the result, the protocol and the APQ outcome, what of
    /// them there is; then the preset, the lag and the display form.
    pub fn summary(&self) -> String {
        let display = self
            .preset
            .as_ref()
            .map(|_| self.display.label().to_owned());
        let parts = [
            &self.result,
            &self.protocol,
            &self.apq,
            &self.preset,
            &self.lag,
            &display,
        ];
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
    json_lines(view.shown_data(), "No data.", ui)
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
            preset: None,
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

    fn subgraph_record() -> GraphqlRecord {
        let mut record = record();
        record.result = Some("ok".into());
        record.preset = Some(rustman_core::run::PresetRecord {
            name: "subgraph".into(),
            scalars: [("token.totalSupply".to_owned(), "BigInt".to_owned())].into(),
            freshness: Some(rustman_core::run::SubgraphFacts {
                indexed_block: Some(1000),
                head: Some(1012),
                rpc: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        record
    }

    const OWNER: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";

    fn subgraph_body() -> String {
        json!({"data": {"token": {
            "owner": OWNER, "decimals": 18, "totalSupply": "1000000000000000000000",
            "transactionHash": "0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060"
        }}})
        .to_string()
    }

    #[test]
    fn a_preset_formats_data_and_cycles_units_and_raw() {
        let mut view = GraphqlView::new(&subgraph_record(), Some(subgraph_body().as_bytes()));
        let ui = UiOptions::default();
        let shown = text(&lines(&view, "Data", &ui));
        assert!(
            shown.starts_with(
                "query Me · ok · preset subgraph · lag 12 blocks · formatted
"
            ),
            "{shown}"
        );
        assert!(
            shown.contains("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            "{shown}"
        );
        assert!(view.cycle_display());
        let units = text(&lines(&view, "Data", &ui));
        assert!(units.contains("\"1000 (18 decimals)\""), "{units}");
        assert!(view.cycle_display());
        let raw = text(&lines(&view, "Data", &ui));
        assert!(raw.contains(OWNER) && raw.contains("· raw\n"), "{raw}");
        assert_eq!(view.links.len(), 2);
        assert!(!GraphqlView::new(&record(), None).cycle_display());
    }

    #[test]
    fn formatted_values_and_links_that_spell_a_secret_stay_redacted() {
        let secret = rustman_core::Secret::new(
            "owner",
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".to_owned(),
        );
        let redactor = Redactor::new().with(&secret);
        let view = GraphqlView::new(&subgraph_record(), Some(subgraph_body().as_bytes()))
            .redacted(&redactor);
        let shown = view.data.join("\n");
        assert!(shown.contains("«redacted:owner»"), "{shown}");
        assert!(!shown.contains("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"));
        assert_eq!(
            view.links.len(),
            1,
            "the address link would show the secret"
        );
    }
}
