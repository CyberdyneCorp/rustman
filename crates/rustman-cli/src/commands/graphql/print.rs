//! How `rustman graphql query` prints a result: `data`, `errors` and
//! `extensions` apart for an operation, the events for a subscription, or
//! the raw body when the response is not a GraphQL result. With a
//! blockchain preset, `data` shows its scalars formatted (display only;
//! `--raw` shows them as received). Everything printed is redacted.

use std::io::{self, Write};

use rustman_core::run::{GraphqlRecord, PresetRecord, RequestRecord, SubscriptionRecord};
use rustman_core::{ConfigError, Redactor};
use rustman_evm::gql::{Display, format_data};
use rustman_graphql::{GraphqlResponse, ResponseError};
use rustman_http::Exchange;
use serde_json::{Value, json};

/// A result ready to print.
pub struct Shown<'a> {
    record: &'a RequestRecord,
    status: u16,
    /// The response body, redacted.
    body: Vec<u8>,
    response: Option<GraphqlResponse>,
    redactor: &'a Redactor,
    display: Display,
}

impl<'a> Shown<'a> {
    pub fn new(
        record: &'a RequestRecord,
        exchange: &Exchange,
        redactor: &'a Redactor,
        display: Display,
    ) -> Self {
        let body = redactor.bytes(&exchange.body).into_owned();
        Self {
            record,
            status: exchange.status,
            response: GraphqlResponse::parse(&body),
            body,
            redactor,
            display,
        }
    }

    fn preset(&self) -> Option<&PresetRecord> {
        self.graphql().and_then(|g| g.preset.as_ref())
    }

    /// A GraphQL result without errors.
    pub fn no_errors(&self) -> bool {
        self.response.as_ref().is_some_and(|r| r.errors.is_empty())
    }

    fn graphql(&self) -> Option<&GraphqlRecord> {
        self.record.graphql.as_ref()
    }

    /// Print on stdout, as text or as one JSON object. A closed stdout
    /// (`| head`) is not an error.
    pub fn write(&self, as_json: bool) -> Result<(), ConfigError> {
        let mut out = io::stdout().lock();
        let written = if as_json {
            let value = self.json();
            serde_json::to_writer_pretty(&mut out, &value)
                .map_err(io::Error::other)
                .and_then(|()| writeln!(out))
        } else {
            self.text(&mut out)
        };
        let _ = written.and_then(|()| out.flush());
        Ok(())
    }

    fn json(&self) -> Value {
        let graphql = self.graphql();
        let mut value = json!({
            "status": self.status,
            "result": graphql.and_then(|g| g.result.clone()),
            "duration_ms": self.record.duration_ms,
        });
        if let Some(subscription) = graphql.and_then(|g| g.subscription.as_ref()) {
            value["protocol"] = json!(graphql.and_then(|g| g.protocol.clone()));
            value["ended"] = json!(subscription.ended);
            value["event_count"] = json!(subscription.event_count);
            value["failure"] = json!(subscription.failure);
            value["events"] = json!(subscription.events);
            return value;
        }
        if let Some(preset) = self.preset() {
            value["preset"] = preset_json(preset);
        }
        match &self.response {
            Some(response) => {
                value["apq"] = json!(graphql.and_then(|g| g.apq.clone()));
                value["data"] = json!(response.data);
                value["errors"] = json!(response.errors);
                value["extensions"] = json!(response.extensions);
            }
            None => value["body"] = json!(String::from_utf8_lossy(&self.body)),
        }
        value
    }

    fn text(&self, out: &mut dyn Write) -> io::Result<()> {
        let graphql = self.graphql();
        if let Some(subscription) = graphql.and_then(|g| g.subscription.as_ref()) {
            let protocol = graphql
                .and_then(|g| g.protocol.as_deref())
                .unwrap_or_default();
            return self.events(out, protocol, subscription);
        }
        let mut status = vec![format!("HTTP {}", self.status)];
        status.extend(graphql.and_then(|g| g.result.clone()));
        status.push(format!("{:.0} ms", self.record.duration_ms));
        status.extend(graphql.and_then(|g| g.apq.clone()));
        if let Some(preset) = self.preset() {
            status.push(format!("preset {}", preset.name));
            status.extend(preset.freshness.as_ref().map(|f| f.lag_text()));
        }
        writeln!(out, "{}", status.join(" · "))?;
        let Some(response) = &self.response else {
            out.write_all(&self.body)?;
            return writeln!(out);
        };
        let data = response.data.as_ref().map(|data| self.shown_data(data));
        result(out, data.as_deref(), response)
    }

    /// `data` as shown: formatted per the preset, redacted again (a
    /// formatted value can spell a secret the raw one did not).
    fn shown_data(&self, data: &Value) -> String {
        let formatted = match self.preset() {
            Some(preset) => format_data(data, &preset.scalars, self.display),
            None => data.clone(),
        };
        self.redactor.text(&pretty(&formatted)).into_owned()
    }

    fn events(
        &self,
        out: &mut dyn Write,
        protocol: &str,
        subscription: &SubscriptionRecord,
    ) -> io::Result<()> {
        let count = match subscription.event_count {
            1 => "1 event".to_owned(),
            n => format!("{n} events"),
        };
        writeln!(
            out,
            "{protocol} · {count} · ended: {} · {:.0} ms",
            subscription.ended, self.record.duration_ms
        )?;
        if let Some(failure) = &subscription.failure {
            writeln!(out, "failed: {failure}")?;
        }
        for (i, event) in subscription.events.iter().enumerate() {
            writeln!(
                out,
                "event {} at {} (+{:.0} ms):",
                i + 1,
                event.ts,
                event.elapsed_ms
            )?;
            writeln!(out, "{}", pretty(&event.payload))?;
        }
        Ok(())
    }
}

/// The preset of `--json` output: its name, warning and freshness.
fn preset_json(preset: &PresetRecord) -> Value {
    let mut value = json!({
        "name": preset.name,
        "explicit": preset.explicit,
        "warning": preset.warning,
    });
    if let Some(facts) = &preset.freshness {
        value["freshness"] = json!(facts);
        value["freshness"]["lag"] = json!(facts.lag());
    }
    value
}

/// `data` (already shown as text), `errors` and `extensions`, each under
/// its own heading.
fn result(out: &mut dyn Write, data: Option<&str>, response: &GraphqlResponse) -> io::Result<()> {
    if let Some(data) = data {
        writeln!(out, "data:\n{data}")?;
    }
    if !response.errors.is_empty() {
        writeln!(out, "errors:")?;
        for error in &response.errors {
            writeln!(out, "  - {}", error_line(error))?;
        }
    }
    if let Some(extensions) = &response.extensions {
        writeln!(
            out,
            "extensions:\n{}",
            pretty(&Value::Object(extensions.clone()))
        )?;
    }
    Ok(())
}

/// `message (path user.email, at 1:22, code FORBIDDEN)`.
fn error_line(error: &ResponseError) -> String {
    let mut details = Vec::new();
    if !error.path.is_empty() {
        details.push(format!("path {}", error.path_text()));
    }
    let at: Vec<String> = error
        .locations
        .iter()
        .map(|l| format!("{}:{}", l.line, l.column))
        .collect();
    if !at.is_empty() {
        details.push(format!("at {}", at.join(", ")));
    }
    if let Some(code) = error.code() {
        details.push(format!("code {code}"));
    }
    if details.is_empty() {
        error.message.clone()
    } else {
        format!("{} ({})", error.message, details.join(", "))
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_show_their_path_location_and_code() {
        let body = br#"{"errors":[{"message":"forbidden","path":["user","email"],
            "locations":[{"line":1,"column":22}],"extensions":{"code":"FORBIDDEN"}},
            {"message":"plain"}]}"#;
        let response = GraphqlResponse::parse(body).unwrap();
        assert_eq!(
            error_line(&response.errors[0]),
            "forbidden (path user.email, at 1:22, code FORBIDDEN)"
        );
        assert_eq!(error_line(&response.errors[1]), "plain");
    }
}
