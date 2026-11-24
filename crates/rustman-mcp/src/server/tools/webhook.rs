//! `list_webhook_captures` (agent-mcp-interface "Conditional protocol
//! tools"), registered when the build includes the `webhook` feature (the
//! webhook-inspector capability).
//!
//! The agent server runs no listener: it reads the captures a human saved
//! to request history from the webhook inspector (inbound entries, request
//! `webhook`), which were redacted when they were saved and are redacted
//! again on the way out. Read-only: nothing is replayed or sent.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustman_core::history::{Entry, HistoryStore, timestamp_millis};
use rustman_webhook::save::INBOUND_REQUEST;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::execute::HeaderView;
use super::history::since_millis;
use super::{Call, output};
use crate::server::context::{AgentContext, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::limits::{BodyView, paginate, snapshot};

/// The name the tool is registered under.
pub const TOOL: &str = "list_webhook_captures";

/// The signature verdicts a capture can have.
const SIGNATURES: [&str; 4] = ["valid", "invalid", "missing", "expired"];

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWebhookCapturesArgs {
    /// Only captures whose path and query contain this text (any case).
    pub path: Option<String>,
    /// Only this method.
    pub method: Option<String>,
    /// Only captures with this signature verdict: `valid`, `invalid`,
    /// `missing` or `expired`.
    pub verdict: Option<String>,
    /// Only captures saved at or after this time: an age (`30s`, `15m`,
    /// `1h`, `2d`, `1w`) or an RFC 3339 timestamp.
    pub since: Option<String>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Items per page (default 50).
    pub limit: Option<usize>,
}

/// One saved capture, redacted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WebhookCapture {
    /// The history entry id.
    pub id: String,
    /// The capture's number in the listener that received it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<u64>,
    /// When the listener received it (RFC 3339).
    pub received_at: String,
    pub method: String,
    /// The path and query, as received.
    pub target: String,
    /// The status the listener answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The client address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    /// The mock that answered it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,
    /// `valid`, `invalid`, `missing` or `expired`, when a verifier checked
    /// it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub headers: Vec<HeaderView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyView>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WebhookCaptureList {
    /// Newest first.
    pub items: Vec<WebhookCapture>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many captures match, over every page.
    pub total: usize,
}

/// The filters of a listing, parsed.
struct Filters {
    path: Option<String>,
    method: Option<String>,
    signature: Option<String>,
    /// Milliseconds since the epoch.
    since: Option<u64>,
}

impl Filters {
    fn of(args: &ListWebhookCapturesArgs) -> Result<Self, ToolError> {
        let signature = args.verdict.as_deref().map(str::to_ascii_lowercase);
        if let Some(verdict) = &signature
            && !SIGNATURES.contains(&verdict.as_str())
        {
            return Err(ToolError::new(
                ErrorCode::InvalidInput,
                format!("invalid verdict '{verdict}' (use valid, invalid, missing or expired)"),
            ));
        }
        Ok(Self {
            path: args.path.as_deref().map(str::to_lowercase),
            method: args.method.clone(),
            signature,
            since: args.since.as_deref().map(since_millis).transpose()?,
        })
    }

    fn keeps(&self, entry: &Entry) -> bool {
        let path = self
            .path
            .as_deref()
            .is_none_or(|text| entry.sent.url.to_lowercase().contains(text));
        let method = self
            .method
            .as_deref()
            .is_none_or(|m| entry.sent.method.eq_ignore_ascii_case(m));
        let signature = self
            .signature
            .as_deref()
            .is_none_or(|s| inbound(entry, "signature").as_str() == Some(s));
        let since = self
            .since
            .is_none_or(|cutoff| timestamp_millis(&entry.id).unwrap_or(0) >= cutoff);
        path && method && signature && since
    }
}

/// The `record.inbound.<name>` of a capture entry.
fn inbound<'e>(entry: &'e Entry, name: &str) -> &'e Value {
    &entry.record["inbound"][name]
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn capture(ctx: &AgentContext, entry: Entry) -> WebhookCapture {
    let size = inbound(&entry, "body_size").as_u64();
    let body = match (&entry.sent.body, inbound(&entry, "body_base64").as_str()) {
        (_, Some(encoded)) => STANDARD.decode(encoded).ok().map(|bytes| {
            let size = size.unwrap_or(bytes.len() as u64);
            ctx.bodies().view(bytes.into(), size, true)
        }),
        (Some(text), None) => {
            let size = size.unwrap_or(text.len() as u64);
            Some(ctx.bodies().view(text.clone().into(), size, false))
        }
        (None, None) => None,
    };
    WebhookCapture {
        capture_id: inbound(&entry, "capture_id").as_u64(),
        received_at: text(inbound(&entry, "received_at"))
            .unwrap_or_else(|| entry.timestamp.clone()),
        remote: text(inbound(&entry, "remote")),
        http_version: text(inbound(&entry, "http_version")),
        mock: text(inbound(&entry, "mock")),
        signature: text(inbound(&entry, "signature")),
        id: entry.id,
        method: entry.sent.method,
        target: entry.sent.url,
        status: entry.status,
        headers: entry
            .sent
            .headers
            .into_iter()
            .map(|h| HeaderView {
                name: h.name,
                value: h.value,
            })
            .collect(),
        body,
    }
}

pub fn list_webhook_captures(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ListWebhookCapturesArgs = call.args()?;
    let filters = Filters::of(&args)?;
    let ctx = &call.ctx;
    let ws = ctx.workspace();
    let store = HistoryStore::for_workspace(&ws.root, &ws.settings.history);
    let listing = store
        .list()
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let found: Vec<Entry> = listing
        .entries
        .into_iter()
        .filter(|e| e.request == INBOUND_REQUEST && filters.keeps(e))
        .collect();
    let total = found.len();
    let keys: Vec<String> = found.iter().map(|e| e.id.clone()).collect();
    let page = paginate(found, snapshot(&keys), args.cursor.as_deref(), args.limit)?;
    let shown = page.items.len();
    let result = WebhookCaptureList {
        items: page.items.into_iter().map(|e| capture(ctx, e)).collect(),
        next_cursor: page.next_cursor,
        total,
    };
    output(
        &result,
        format!("{shown} of {total} saved webhook captures, newest first"),
    )
}
