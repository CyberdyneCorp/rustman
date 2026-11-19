//! Resolving a `kind: jsonrpc` request before it is sent.
//!
//! `{{name}}` is interpolated in the endpoint, headers (inherited and own),
//! auth fields, settings strings and the string values of `params` (keys
//! and method names are kept as written). Every missing name is reported
//! at once, and nothing can be sent unless resolution succeeds.

use std::fmt;

use serde_json::{Map, Value};

use super::request::{
    ResolvedBody, ResolvedRequest, auth, contain_files, contain_jwt_key, inherit_client_identity,
    pairs, settings, tls_files,
};
use super::resolver::{Interpolator, ResolveError, Resolver};
use crate::model::{CallFields, JsonrpcRequest, Request, RetrySettings, SubscriptionSpec};
use crate::redact::Redactor;
use crate::workspace::RequestRef;

/// One call with its params interpolated.
#[derive(Clone, PartialEq)]
pub struct ResolvedCall {
    pub method: String,
    /// A JSON array or object; `None` leaves `params` out of the envelope.
    pub params: Option<Value>,
    /// The explicit id; `None` takes the connection's next id.
    pub id: Option<Value>,
    pub notification: bool,
}

impl ResolvedCall {
    /// A call built in code (the CLI), with no explicit id.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            method: method.into(),
            params,
            id: None,
            notification: false,
        }
    }
}

/// `Debug` hides the params, which may hold secrets; a whole
/// [`ResolvedJsonrpc`] prints them redacted instead.
impl fmt::Debug for ResolvedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedCall")
            .field("method", &self.method)
            .field("params", &self.params.as_ref().map(|_| "…"))
            .field("id", &self.id)
            .field("notification", &self.notification)
            .finish()
    }
}

/// A jsonrpc request with every placeholder replaced.
///
/// `http` carries the endpoint, headers, auth, settings and TLS files with
/// no body; the transport adds the envelope. Its `secrets` hold every
/// secret used anywhere in the request, params included, so
/// [`ResolvedJsonrpc::redactor`] covers the whole exchange. The `Debug`
/// output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedJsonrpc {
    pub http: ResolvedRequest,
    pub calls: Vec<ResolvedCall>,
    /// Sent as a JSON array, even with one call.
    pub batch: bool,
    pub subscription: Option<SubscriptionSpec>,
    pub idempotent: bool,
    pub retry: RetrySettings,
}

impl ResolvedJsonrpc {
    /// A request built in code: `calls` to `http`'s URL.
    pub fn new(http: ResolvedRequest, calls: Vec<ResolvedCall>, batch: bool) -> Self {
        Self {
            http,
            calls,
            batch,
            subscription: None,
            idempotent: false,
            retry: RetrySettings::default(),
        }
    }

    /// The redactor for everything shown or recorded about this request.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(&self.http)
    }
}

impl fmt::Debug for ResolvedJsonrpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let calls: Vec<_> = self
            .calls
            .iter()
            .map(|c| (&c.method, &c.params, &c.id, c.notification))
            .collect();
        let raw = format!(
            "ResolvedJsonrpc {{ http: {:?}, calls: {calls:?}, batch: {}, subscription: {:?}, idempotent: {}, retry: {:?} }}",
            self.http, self.batch, self.subscription, self.idempotent, self.retry
        );
        f.write_str(&self.redactor().text(&raw))
    }
}

/// Resolve the jsonrpc request `request` through `resolver`.
pub fn resolve_jsonrpc(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedJsonrpc, ResolveError> {
    let Request::Jsonrpc(jsonrpc) = request.request() else {
        let kind = request.request().kind();
        return Err(ResolveError::Unsupported(format!(
            "request kind '{kind}' is not a jsonrpc request"
        )));
    };
    let mut i = resolver.interpolator();
    let mut settings = settings(&mut i, jsonrpc.http_settings().as_ref());
    let mut tls = tls_files(&settings);
    inherit_client_identity(&mut i, request, &mut settings, &mut tls)?;
    let http = ResolvedRequest {
        method: "POST".to_owned(),
        url: i.text(&jsonrpc.endpoint),
        query: Vec::new(),
        headers: pairs(&mut i, &request.effective_headers()),
        body: ResolvedBody::None,
        auth: request.effective_auth().map(|a| auth(&mut i, a)),
        settings,
        tls,
        secrets: Vec::new(),
    };
    let calls: Vec<ResolvedCall> = jsonrpc.calls().iter().map(|c| call(&mut i, c)).collect();
    let ((mut http, calls), secrets) = i.finish((http, calls))?;
    http.secrets = secrets;
    contain_files(request, &mut http)?;
    contain_jwt_key(request, &mut http)?;
    Ok(resolved(jsonrpc, http, calls))
}

fn resolved(
    jsonrpc: &JsonrpcRequest,
    http: ResolvedRequest,
    calls: Vec<ResolvedCall>,
) -> ResolvedJsonrpc {
    ResolvedJsonrpc {
        http,
        calls,
        batch: jsonrpc.batch.is_some(),
        subscription: jsonrpc.subscription.as_ref().map(|s| s.get().clone()),
        idempotent: jsonrpc.is_idempotent(),
        retry: jsonrpc.retry(),
    }
}

fn call(i: &mut Interpolator<'_, '_>, fields: &CallFields<'_>) -> ResolvedCall {
    ResolvedCall {
        method: fields.method.unwrap_or_default().to_owned(),
        params: fields.params.map(|p| string_values(i, p.get())),
        id: fields.id.map(|id| id.get().clone()),
        notification: fields.notification,
    }
}

/// `value` with every string interpolated; keys and other scalars are
/// kept, so numbers and hex strings go out exactly as written.
fn string_values(i: &mut Interpolator<'_, '_>, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(i.text(text).text),
        Value::Array(items) => Value::Array(items.iter().map(|v| string_values(i, v)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), string_values(i, v)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}
