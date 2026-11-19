//! Resolving the realtime kinds (`kind: ws`, `kind: sse`) before a session
//! opens.
//!
//! `{{name}}` is interpolated in the URL, headers (inherited and own), auth
//! fields, TLS strings and, for WebSocket, the string values of steps:
//! text and ping payloads, base64 and file paths of binary frames, the
//! string values of JSON frames, `wait_for` paths, patterns and expected
//! values, and close reasons. For SSE, the body is interpolated as an
//! HTTP body is. Every missing name is reported at once, and every secret
//! used anywhere lands in `http.secrets`, so [`Redactor::for_request`]
//! covers the whole session.

use std::fmt;

use serde_json::Value;

use super::graphql::string_values;
use super::request::{
    ResolvedBody, ResolvedFile, ResolvedPair, ResolvedRequest, auth, body, contain_files,
    contain_jwt_key, inherit_client_identity, pairs, settings, tls_files,
};
use super::resolver::{Interpolator, ResolveError, Resolved, Resolver};
use crate::model::{
    BinarySource, CloseStep, Frame, JsonAssertion, RealtimeLimits, Reconnect, Request,
    RequestSettings, SSE_ACCEPT, SseRequest, Step, WaitFor, WsRequest,
};
use crate::paths::PathEscape;
use crate::redact::Redactor;
use crate::workspace::RequestRef;

/// A frame with its strings interpolated. A binary frame keeps its base64
/// text (decoded when sent) or its contained file.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedFrame {
    Text(String),
    Json(Value),
    Binary(ResolvedBinary),
    Ping(String),
}

impl ResolvedFrame {
    /// The frame type as the timeline names it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Json(_) => "json",
            Self::Binary(_) => "binary",
            Self::Ping(_) => "ping",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedBinary {
    Base64(String),
    File(ResolvedFile),
}

/// A step with its strings interpolated.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedStep {
    Send(ResolvedFrame),
    WaitFor(Box<WaitFor>),
    Sleep(u64),
    Close(CloseStep),
}

/// A `kind: ws` request with every placeholder replaced. `http` carries
/// the handshake: `GET`, the `ws(s)://` URL, headers, auth and TLS
/// settings, and every secret of the session. The `Debug` output is
/// redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedWs {
    pub http: ResolvedRequest,
    pub subprotocols: Vec<String>,
    pub steps: Vec<ResolvedStep>,
    pub limits: RealtimeLimits,
}

/// A `kind: sse` request with every placeholder replaced: `http` is the
/// request that opens the stream (with `Accept: text/event-stream` unless
/// the file sets `Accept`). The `Debug` output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedSse {
    pub http: ResolvedRequest,
    pub last_event_id: Option<String>,
    pub reconnect: Reconnect,
    pub limits: RealtimeLimits,
}

/// A resolved realtime request.
#[derive(Clone, PartialEq)]
pub enum ResolvedRealtime {
    Ws(ResolvedWs),
    Sse(ResolvedSse),
}

impl ResolvedRealtime {
    /// The request that opens the session.
    pub fn http(&self) -> &ResolvedRequest {
        match self {
            Self::Ws(ws) => &ws.http,
            Self::Sse(sse) => &sse.http,
        }
    }

    pub fn limits(&self) -> &RealtimeLimits {
        match self {
            Self::Ws(ws) => &ws.limits,
            Self::Sse(sse) => &sse.limits,
        }
    }

    /// `ws` or `sse`.
    pub fn protocol(&self) -> &'static str {
        match self {
            Self::Ws(_) => "ws",
            Self::Sse(_) => "sse",
        }
    }

    /// The redactor for everything shown or recorded about this session.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(self.http())
    }
}

impl fmt::Debug for ResolvedWs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedWs {{ http: {:?}, subprotocols: {:?}, steps: {:?}, limits: {:?} }}",
            self.http, self.subprotocols, self.steps, self.limits
        );
        f.write_str(&Redactor::for_request(&self.http).text(&raw))
    }
}

impl fmt::Debug for ResolvedSse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedSse {{ http: {:?}, last_event_id: {:?}, reconnect: {:?}, limits: {:?} }}",
            self.http, self.last_event_id, self.reconnect, self.limits
        );
        f.write_str(&Redactor::for_request(&self.http).text(&raw))
    }
}

impl fmt::Debug for ResolvedRealtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ws(ws) => ws.fmt(f),
            Self::Sse(sse) => sse.fmt(f),
        }
    }
}

/// Resolve the `ws` or `sse` request `request` through `resolver`.
pub fn resolve_realtime(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedRealtime, ResolveError> {
    match request.request() {
        Request::Ws(ws) => resolve_ws(request, ws, resolver).map(ResolvedRealtime::Ws),
        Request::Sse(sse) => resolve_sse(request, sse, resolver).map(ResolvedRealtime::Sse),
        other => Err(ResolveError::Unsupported(format!(
            "request kind '{}' is not a realtime request",
            other.kind()
        ))),
    }
}

fn resolve_ws(
    request: &RequestRef<'_>,
    ws: &WsRequest,
    resolver: &Resolver<'_>,
) -> Result<ResolvedWs, ResolveError> {
    let mut i = resolver.interpolator();
    let http = handshake(&mut i, request, ws.url.get(), ws.http_settings())?;
    let steps: Vec<ResolvedStep> = ws.step_list().iter().map(|s| step(&mut i, s)).collect();
    let ((mut http, mut steps), secrets) = i.finish((http, steps))?;
    http.secrets = secrets;
    contain_files(request, &mut http)?;
    contain_jwt_key(request, &mut http)?;
    contain_frames(request, &http, &mut steps)?;
    Ok(ResolvedWs {
        http,
        subprotocols: ws.subprotocols.clone().unwrap_or_default(),
        steps,
        limits: ws.effective_limits(),
    })
}

fn resolve_sse(
    request: &RequestRef<'_>,
    sse: &SseRequest,
    resolver: &Resolver<'_>,
) -> Result<ResolvedSse, ResolveError> {
    let mut i = resolver.interpolator();
    let mut http = handshake(&mut i, request, sse.url.get(), sse.http_settings())?;
    http.method = sse.effective_method().to_owned();
    http.body = body(&mut i, sse.body.as_ref());
    let has_accept = http
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case("accept"));
    if !has_accept {
        http.headers.push(ResolvedPair {
            name: Resolved::literal("Accept"),
            value: Resolved::literal(SSE_ACCEPT),
        });
    }
    let last_event_id = sse.last_event_id.as_deref().map(|id| i.text(id).text);
    let ((mut http, last_event_id), secrets) = i.finish((http, last_event_id))?;
    http.secrets = secrets;
    contain_files(request, &mut http)?;
    contain_jwt_key(request, &mut http)?;
    Ok(ResolvedSse {
        http,
        last_event_id,
        reconnect: sse.effective_reconnect(),
        limits: sse.effective_limits(),
    })
}

/// The request that opens the session: `GET` with the URL, headers, auth
/// and TLS settings interpolated, and no body yet.
fn handshake(
    i: &mut Interpolator<'_, '_>,
    request: &RequestRef<'_>,
    url: &str,
    tls: Option<RequestSettings>,
) -> Result<ResolvedRequest, ResolveError> {
    let mut settings = settings(i, tls.as_ref());
    let mut tls = tls_files(&settings);
    inherit_client_identity(i, request, &mut settings, &mut tls)?;
    Ok(ResolvedRequest {
        method: "GET".to_owned(),
        url: i.text(url),
        query: Vec::new(),
        headers: pairs(i, &request.effective_headers()),
        body: ResolvedBody::None,
        auth: request.effective_auth().map(|a| auth(i, a)),
        settings,
        tls,
        secrets: Vec::new(),
    })
}

fn step(i: &mut Interpolator<'_, '_>, step: &Step) -> ResolvedStep {
    match step {
        Step::Send(f) => ResolvedStep::Send(frame(i, f)),
        Step::WaitFor(w) => ResolvedStep::WaitFor(Box::new(wait_for(i, w))),
        Step::Sleep(ms) => ResolvedStep::Sleep(*ms),
        Step::Close(c) => ResolvedStep::Close(CloseStep {
            code: c.code,
            reason: c.reason.as_deref().map(|r| i.text(r).text),
        }),
    }
}

fn frame(i: &mut Interpolator<'_, '_>, frame: &Frame) -> ResolvedFrame {
    match frame {
        Frame::Text(text) => ResolvedFrame::Text(i.text(text).text),
        Frame::Json(value) => ResolvedFrame::Json(string_values(i, value)),
        Frame::Binary(BinarySource::Base64(data)) => {
            ResolvedFrame::Binary(ResolvedBinary::Base64(i.text(data).text))
        }
        Frame::Binary(BinarySource::File(path)) => ResolvedFrame::Binary(ResolvedBinary::File(
            ResolvedFile::unchecked(i.text(path.get()).text),
        )),
        Frame::Ping(data) => ResolvedFrame::Ping(i.text(data).text),
    }
}

fn wait_for(i: &mut Interpolator<'_, '_>, wait: &WaitFor) -> WaitFor {
    WaitFor {
        json: wait.json.as_ref().map(|j| json_matcher(i, j)),
        regex: wait.regex.as_deref().map(|r| i.text(r).text),
        timeout_ms: wait.timeout_ms,
    }
}

/// A `wait_for` JSON check with its path, pattern and expected values
/// interpolated.
fn json_matcher(
    i: &mut Interpolator<'_, '_>,
    json: &crate::model::Checked<JsonAssertion>,
) -> crate::model::Checked<JsonAssertion> {
    let mut value = |v: &Option<Value>| v.as_ref().map(|v| string_values(i, v));
    let resolved = JsonAssertion {
        equals: value(&json.equals),
        not_equals: value(&json.not_equals),
        contains: value(&json.contains),
        gt: value(&json.gt),
        gte: value(&json.gte),
        lt: value(&json.lt),
        lte: value(&json.lte),
        length: value(&json.length),
        path: i.text(&json.path).text,
        matches: json.matches.as_deref().map(|m| i.text(m).text),
        exists: json.exists,
        value_type: json.value_type.clone(),
    };
    crate::model::Checked::new(resolved).unwrap_or_else(|_| json.clone())
}

/// Check every binary file frame stays inside the workspace. The error
/// shows the resolved path with any secret redacted.
fn contain_frames(
    request: &RequestRef<'_>,
    http: &ResolvedRequest,
    steps: &mut [ResolvedStep],
) -> Result<(), ResolveError> {
    for step in steps {
        if let ResolvedStep::Send(ResolvedFrame::Binary(ResolvedBinary::File(file))) = step {
            match request.contain_file(&file.raw) {
                Ok(path) => file.path = path,
                Err(PathEscape(path)) => {
                    let shown = Redactor::for_request(http).text(&path).into_owned();
                    return Err(PathEscape(shown).into());
                }
            }
        }
    }
    Ok(())
}
