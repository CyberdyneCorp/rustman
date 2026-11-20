//! Webhook payloads: `minijinja` templates that render JSON. The built-in
//! `slack`, `discord` and `generic` templates can be replaced per
//! workspace with `monitors/templates/<name>.json.j2`.
//!
//! A template sees `monitor`, `environment`, `status` (`firing`,
//! `reminder`, `resolved` or `test`), `rule`, `title`, `text`, `failure`,
//! `at` (RFC 3339), `timestamp` (Unix seconds), `duration_seconds`,
//! `color` (`#rrggbb`) and `color_int`, and the `tojson` filter. What it
//! renders must be JSON; every string in it is redacted before it is sent.

use std::path::Path;

use minijinja::{AutoEscape, Environment, Value};
use rustman_core::Redactor;

use super::{Alert, AlertKind};
use crate::alerts::WebhookTemplate;

/// Where a workspace overrides the built-in templates.
pub const TEMPLATES_DIR: &str = "monitors/templates";

const SLACK: &str = include_str!("templates/slack.json.j2");
const DISCORD: &str = include_str!("templates/discord.json.j2");
const GENERIC: &str = include_str!("templates/generic.json.j2");

/// The payload of `alert` for `template`, from the workspace's override
/// when there is one, redacted.
pub fn render(
    root: &Path,
    template: WebhookTemplate,
    alert: &Alert,
    redactor: &Redactor,
) -> Result<serde_json::Value, String> {
    let (name, builtin) = match template {
        WebhookTemplate::Slack => ("slack", SLACK),
        WebhookTemplate::Discord => ("discord", DISCORD),
        WebhookTemplate::Generic => ("generic", GENERIC),
    };
    let file = format!("{name}.json.j2");
    let path = root.join(TEMPLATES_DIR).join(&file);
    let source = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => builtin.to_owned(),
        Err(e) => return Err(format!("{TEMPLATES_DIR}/{file}: {e}")),
    };
    let rendered = render_source(&source, alert)
        .map_err(|e| format!("template {name}: {}", redactor.text(&e)))?;
    let mut payload: serde_json::Value = serde_json::from_str(&rendered)
        .map_err(|e| format!("template {name} did not render JSON: {e}"))?;
    redactor.json(&mut payload);
    Ok(payload)
}

fn render_source(source: &str, alert: &Alert) -> Result<String, String> {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.add_filter("tojson", |value: Value| {
        serde_json::to_string(&value).map_err(|e| {
            minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string())
        })
    });
    env.add_template("payload", source)
        .map_err(|e| e.to_string())?;
    let template = env.get_template("payload").map_err(|e| e.to_string())?;
    let context = Value::from(minijinja::value::Serde(context(alert)));
    template.render(context).map_err(|e| e.to_string())
}

fn context(alert: &Alert) -> serde_json::Value {
    let color = match alert.kind {
        AlertKind::Firing | AlertKind::Reminder => 0x00d9_3025,
        AlertKind::Resolved => 0x0018_8038,
        AlertKind::Test => 0x001a_73e8,
    };
    serde_json::json!({
        "monitor": alert.monitor,
        "environment": alert.environment,
        "status": alert.kind.as_str(),
        "rule": alert.rule,
        "title": alert.title(),
        "text": alert.text(),
        "failure": alert.failure,
        "at": alert.at.to_string(),
        "timestamp": alert.at.as_second(),
        "duration_seconds": alert.duration.map(|d| d.as_secs()),
        "color": format!("#{color:06x}"),
        "color_int": color,
    })
}
