//! What an MCP operation adds to its request record (mcp-client "History
//! for MCP exchanges"): the server, transport and protocol version, the
//! method and params, the result or error, and the notifications received
//! while it ran.
//!
//! Records are redacted with [`McpRecord::redacted`] before they reach any
//! sink, like every other text of a [`super::RequestRecord`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graphql::redact_value;
use crate::redact::Redactor;

/// How many notifications of one operation a record keeps.
pub const MAX_RECORDED_NOTIFICATIONS: usize = 200;

/// The MCP side of one executed request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct McpRecord {
    /// The server's name in the workspace (or as given on the command line).
    pub server: String,
    /// `stdio`, `streamable HTTP` or `legacy SSE`.
    pub transport: String,
    /// The negotiated protocol version; `None` when connecting failed.
    pub protocol_version: Option<String>,
    /// The JSON-RPC method, such as `tools/call`.
    pub method: String,
    /// The params sent, redacted.
    pub params: Value,
    /// The result answered, redacted; `None` on an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `ok`, `tool error`, `protocol error <code>`, `cancelled` or `error`.
    pub outcome: String,
    pub duration_ms: f64,
    /// Every notification received while the operation ran.
    pub notification_count: usize,
    /// The first [`MAX_RECORDED_NOTIFICATIONS`] of them, in order.
    pub notifications: Vec<Value>,
    /// Input-schema violations the user chose to send anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_override: Vec<String>,
}

impl McpRecord {
    /// Keep `notifications`, at most [`MAX_RECORDED_NOTIFICATIONS`].
    #[must_use]
    pub fn with_notifications(mut self, mut notifications: Vec<Value>) -> Self {
        self.notification_count = notifications.len();
        notifications.truncate(MAX_RECORDED_NOTIFICATIONS);
        self.notifications = notifications;
        self
    }

    /// This record with every secret `redactor` knows replaced.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |t: &str| redactor.text(t).into_owned();
        self.server = text(&self.server);
        self.params = redact_value(&self.params, redactor);
        self.result = self.result.map(|r| redact_value(&r, redactor));
        self.error = self.error.as_deref().map(text);
        self.notifications = self
            .notifications
            .iter()
            .map(|n| redact_value(n, redactor))
            .collect();
        self.validation_override = self.validation_override.iter().map(|v| text(v)).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;
    use serde_json::json;

    #[test]
    fn only_the_first_two_hundred_notifications_are_kept() {
        let notes = (0..250).map(|n| json!({"n": n})).collect();
        let record = McpRecord::default().with_notifications(notes);
        assert_eq!(record.notification_count, 250);
        assert_eq!(record.notifications.len(), MAX_RECORDED_NOTIFICATIONS);
        assert_eq!(record.notifications[199]["n"], 199);
    }

    #[test]
    fn params_results_and_notifications_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("token", "s3cr3t".to_owned()));
        let record = McpRecord {
            server: "files".into(),
            method: "tools/call".into(),
            params: json!({"name": "login", "arguments": {"token": "s3cr3t"}}),
            result: Some(json!({"content": [{"type": "text", "text": "hi s3cr3t"}]})),
            error: Some("echo s3cr3t".into()),
            outcome: "ok".into(),
            validation_override: vec!["/token: s3cr3t".into()],
            ..McpRecord::default()
        }
        .with_notifications(vec![json!({"message": "s3cr3t"})]);
        let shown = serde_json::to_string(&record.redacted(&redactor)).unwrap();
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(shown.contains("«redacted:token»"), "{shown}");
    }
}
