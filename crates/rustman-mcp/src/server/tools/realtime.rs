//! `realtime_listen` (add-realtime-client "Agent tools"): open a WebSocket
//! or SSE session for an agent, optionally send initial frames, and return
//! the messages received, after at most 100 messages or 60 s.
//!
//! A thin adapter over the `rustman-realtime` runner: before a session
//! opens, [`AgentRealtime`] checks the resolved URL against the session's
//! allowlist (a `ws://` URL as its `http://` origin, `wss://` as
//! `https://`), refuses frames unless `[agent].allow_mutations`, and asks
//! the human to confirm them as for any mutation. The engine's runner
//! resolves, asserts, records history and redacts as for every request.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustman_core::model::Request;
use rustman_core::run::TimelineRecord;
use rustman_core::safety::MethodClass;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::{ResolvedRealtime, ResolvedStep};
use rustman_core::workspace::RequestRef;
use rustman_http::runner::{
    RealtimeCall, RealtimeExecutor, RealtimeFuture, RunContext, RunOptions, Runner, Sent,
};
use rustman_realtime::{Bounds, RealtimeRunner, is_message};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::execute::{AssertionView, history, outcome, overrides};
use super::{Call, output};
use crate::server::confirm::Confirm;
use crate::server::context::{AgentContext, Target, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::on_own_thread;
use crate::server::policy::http_url;

/// The name the tool is registered under.
pub const NAME: &str = "realtime_listen";

/// The most messages a call collects.
pub const MAX_MESSAGES: u32 = 100;

/// The longest a call listens.
pub const MAX_DURATION: Duration = Duration::from_secs(60);

/// Why a read-only agent cannot send frames.
pub const FRAMES_NEED_MUTATIONS: &str = "sending frames requires allow_mutations";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RealtimeListenArgs {
    /// A saved `kind: ws` or `kind: sse` request: its id or file. Give
    /// either `path` or `url`.
    pub path: Option<String>,
    /// The URL of an ad-hoc session: `ws://` or `wss://` opens a WebSocket,
    /// `http://` or `https://` an SSE stream. `{{variables}}` resolve in
    /// the session's environment; a URL that starts with one opens an SSE
    /// stream, or a WebSocket when `frames` are given.
    pub url: Option<String>,
    /// Frames to send once an ad-hoc WebSocket is open: a string is a text
    /// frame, any other value is sent as JSON. Sending frames is a
    /// mutation: it needs `[agent].allow_mutations`.
    pub frames: Option<Vec<Value>>,
    /// Stop after this many received messages (at most 100, the default).
    pub max_messages: Option<u32>,
    /// Stop after this many milliseconds (at most 60000, the default).
    pub duration_ms: Option<u64>,
    /// Variables for this session, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

/// One received message, redacted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RealtimeMessageView {
    /// Milliseconds since the session started.
    pub t_ms: u64,
    /// The frame type (`text`, `json`, `binary`) or the SSE event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The SSE event id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The payload as received; binary data in base64.
    pub data: String,
    /// The payload was cut to a 64 KiB preview.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The redacted result of `realtime_listen`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RealtimeListenResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `ws` or `sse`.
    pub protocol: String,
    /// `passed`, `failed` (an assertion), `errored` (the session failed)
    /// or `skipped`.
    pub outcome: String,
    /// Why the session failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How the session ended: `max_messages reached (100)`, `closed by
    /// server: 1000`...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    /// Messages received, including any the timeline dropped.
    pub received: u64,
    pub messages: Vec<RealtimeMessageView>,
    pub assertions: Vec<AssertionView>,
}

/// Listen on a thread of the session's own: the runner's futures are not
/// `Send`.
pub async fn realtime_listen(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { listen_here(&call).await }).await?
}

async fn listen_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: RealtimeListenArgs = call.args()?;
    let ctx = &call.ctx;
    let ws = ctx.workspace();
    let gate = AgentRealtime {
        ctx,
        confirm: &call.peer,
        target: &call.target,
        sessions: RealtimeRunner {
            max_messages: args.max_messages,
            duration: args.duration_ms.map(Duration::from_millis),
            ceiling: Some(Bounds {
                max_messages: u64::from(MAX_MESSAGES),
                duration: MAX_DURATION,
            }),
            ..RealtimeRunner::default()
        },
        refused: Mutex::new(None),
    };
    let options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        snapshots: SnapshotMode::Ci,
        realtime: Some(&gate),
        history: history(ctx),
        ..RunOptions::default()
    };
    let sent = match (&args.path, &args.url) {
        (Some(path), None) if args.frames.is_none() => {
            let saved = super::workspace::find(ctx, path)?;
            if !matches!(saved.request(), Request::Ws(_) | Request::Sse(_)) {
                let message = format!("'{path}' is not a ws or sse request");
                return Err(ToolError::new(ErrorCode::InvalidInput, message));
            }
            send(ctx, &saved, options, &gate).await
        }
        (None, Some(url)) => {
            // Frames are WebSocket frames, whatever a `{{variable}}` URL holds.
            let kind = match args.frames {
                Some(_) => "ws",
                None => rustman_realtime::kind_of(url),
            };
            let entry = rustman_realtime::ad_hoc_entry(kind, url, ad_hoc_fields(kind, &args)?)
                .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e.to_string()))?;
            let target = RequestRef {
                root: &ws.root,
                http: &ws.settings.http,
                entry: &entry,
                ancestors: Vec::new(),
            };
            let options = RunOptions {
                ad_hoc: true,
                ..options
            };
            send(ctx, &target, options, &gate).await
        }
        _ => {
            return Err(ToolError::new(
                ErrorCode::InvalidInput,
                "give either `path` (a saved ws or sse request) or `url` with optional `frames`",
            ));
        }
    }?;
    let result = listen_result(&sent);
    let line = match &result.error {
        Some(error) => format!("{} {}: {error}", result.request, result.outcome),
        None => format!(
            "{} {}: {} messages",
            result.request,
            result.outcome,
            result.messages.len()
        ),
    };
    output(&result, line)
}

/// The request-file fields of an ad-hoc session: `frames` as `send` steps.
fn ad_hoc_fields(kind: &str, args: &RealtimeListenArgs) -> Result<Map<String, Value>, ToolError> {
    let mut fields = Map::new();
    let Some(frames) = &args.frames else {
        return Ok(fields);
    };
    if kind != "ws" {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            "`frames` are for WebSocket sessions (a ws:// or wss:// url)",
        ));
    }
    let steps: Vec<Value> = frames
        .iter()
        .map(|frame| match frame {
            Value::String(text) => json!({"send": {"text": text}}),
            other => json!({"send": {"json": other}}),
        })
        .collect();
    fields.insert("steps".into(), Value::Array(steps));
    Ok(fields)
}

/// Run `target` through `gate`. A refusal of the agent policy is the
/// tool's error; nothing was opened.
async fn send<C: Confirm>(
    ctx: &AgentContext,
    target: &RequestRef<'_>,
    options: RunOptions<'_>,
    gate: &AgentRealtime<'_, C>,
) -> Result<Sent, ToolError> {
    let environment = ctx.environment();
    let ws = ctx.workspace();
    let run_ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: environment.as_ref(),
        secrets: ctx.secrets(),
        in_workspace: true,
    };
    let runner = Runner::new(run_ctx, options).map_err(|e| ToolError::internal(e.to_string()))?;
    let runner = ctx.with_providers(runner);
    let sent = runner.send(target, 1).await;
    if let Err(err) = runner.finish() {
        tracing::warn!("cookies: {err}");
    }
    ctx.learn(&sent.redactor);
    match gate.refused() {
        Some(refusal) => Err(refusal),
        None => Ok(sent),
    }
}

/// The realtime executor of agent sessions: the allowlist, the frame rule
/// and the human's confirmation before a session opens, remembering the
/// first refusal so the tool can return its code.
struct AgentRealtime<'a, C: Confirm> {
    ctx: &'a AgentContext,
    confirm: &'a C,
    target: &'a Target,
    sessions: RealtimeRunner,
    refused: Mutex<Option<ToolError>>,
}

impl<C: Confirm> RealtimeExecutor for AgentRealtime<'_, C> {
    fn execute<'a>(&'a self, call: RealtimeCall<'a>) -> RealtimeFuture<'a> {
        Box::pin(async move {
            if let Err(err) = self.authorize(call.resolved).await {
                let message = err.to_string();
                self.refuse(err);
                return Err(message);
            }
            self.sessions.execute(call).await
        })
    }
}

impl<C: Confirm> AgentRealtime<'_, C> {
    /// Whether the session `resolved` may open: frames need
    /// `allow_mutations`; then the agent permissions, the allowlist and,
    /// for frames or an SSE `POST`, the human's confirmation.
    async fn authorize(&self, resolved: &ResolvedRealtime) -> Result<(), ToolError> {
        let http = resolved.http();
        let sends = sends_frames(resolved);
        self.target.set(&http.method, &http.url.text);
        if sends
            && !self
                .ctx
                .agent_settings()
                .permission()
                .allows(MethodClass::Mutation)
        {
            return Err(ToolError::blocked(FRAMES_NEED_MUTATIONS));
        }
        let method = if sends { "POST" } else { http.method.as_str() };
        let url = http_url(&http.url.text);
        self.ctx.authorize(method, &url, self.confirm).await
    }

    fn refuse(&self, err: ToolError) {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        refused.get_or_insert(err);
    }

    fn refused(&self) -> Option<ToolError> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Whether the session sends frames of its own (a WebSocket `send` step).
fn sends_frames(resolved: &ResolvedRealtime) -> bool {
    match resolved {
        ResolvedRealtime::Ws(ws) => ws.steps.iter().any(|s| matches!(s, ResolvedStep::Send(_))),
        ResolvedRealtime::Sse(_) => false,
    }
}

/// The tool result of `sent`, from its redacted record.
fn listen_result(sent: &Sent) -> RealtimeListenResult {
    let record = &sent.record;
    let realtime = record.realtime.as_ref();
    let end = realtime.and_then(|r| r.end.as_ref());
    RealtimeListenResult {
        request: record.id.clone(),
        protocol: realtime.map(|r| r.protocol.clone()).unwrap_or_default(),
        outcome: outcome(record.status).to_owned(),
        error: record.error.clone(),
        end: end.map(|e| e.reason.clone()),
        close_code: end.and_then(|e| e.close_code),
        received: realtime.map_or(0, |r| r.received),
        messages: realtime
            .iter()
            .flat_map(|r| r.timeline.iter())
            .filter(|e| is_message(&e.dir, &e.kind))
            .map(message_view)
            .collect(),
        assertions: record
            .assertions
            .iter()
            .map(|a| AssertionView {
                passed: a.passed(),
                text: a.to_string(),
            })
            .collect(),
    }
}

fn message_view(entry: &TimelineRecord) -> RealtimeMessageView {
    RealtimeMessageView {
        t_ms: entry.t_ms,
        kind: entry.kind.clone(),
        id: entry.id.clone(),
        data: entry.payload.clone(),
        truncated: entry.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_become_send_steps_of_websocket_sessions_only() {
        let args = RealtimeListenArgs {
            frames: Some(vec![json!("hi"), json!({"op": "sub"})]),
            ..RealtimeListenArgs::default()
        };
        let fields = ad_hoc_fields("ws", &args).unwrap();
        assert_eq!(
            fields["steps"],
            json!([{"send": {"text": "hi"}}, {"send": {"json": {"op": "sub"}}}])
        );
        assert!(ad_hoc_fields("sse", &args).is_err());
        assert!(
            ad_hoc_fields("sse", &RealtimeListenArgs::default())
                .unwrap()
                .is_empty()
        );
    }
}
