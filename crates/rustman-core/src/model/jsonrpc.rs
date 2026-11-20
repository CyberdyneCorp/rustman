//! The `kind: jsonrpc` request file: one JSON-RPC 2.0 call, a batch of
//! calls, or a subscription, over HTTP or WebSocket.
//!
//! Sending belongs to the `rustman-jsonrpc` crate; this module only holds
//! the file model and its load-time checks.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::assertion::Assertion;
use super::auth::Auth;
use super::body::{Body, JsonContent};
use super::capture::Capture;
use super::common::{Checked, NameValue, Variables};
use super::http::{HttpRequest, Method, RequestSettings};
use crate::error::{LoadError, Position};
use crate::format::{Located, from_yaml};
use crate::secret::{Hidden, placeholder};

/// The most calls one batch may hold.
pub const MAX_BATCH_CALLS: usize = 100;

/// Events a headless subscription collects when `max_events` is not set.
pub const DEFAULT_SUBSCRIPTION_EVENTS: u32 = 10;

/// How long a headless subscription runs when `max_duration_ms` is not set.
pub const DEFAULT_SUBSCRIPTION_DURATION_MS: u64 = 30_000;

/// Where a notification carries its subscription id by default.
pub const DEFAULT_NOTIFICATION_ID_PATH: &str = "$.params.subscription";

/// Retry backoff when `settings.retry.backoff_ms` is not set.
pub const DEFAULT_RETRY_BACKOFF_MS: u64 = 500;

/// A `kind: jsonrpc` request file. Optional fields stay `None` when absent;
/// the `effective_*` methods apply the defaults. Saving writes `source`
/// back unchanged.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonrpcRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// How the request appears in `rustman docs` output.
    pub docs: Option<super::docs::DocsMeta>,
    /// An `http`, `https`, `ws` or `wss` URL; `{{variables}}` are
    /// interpolated.
    pub endpoint: String,
    /// The method of a single call; a subscription defaults it to
    /// `subscription.subscribe`.
    pub method: Option<String>,
    /// A list (positional) or a map (named); string values are
    /// interpolated.
    pub params: Option<Located<Value>>,
    /// An explicit id (string or integer); sequential ids otherwise.
    pub id: Option<Located<Value>>,
    pub notification: Option<bool>,
    /// Several calls sent as one JSON array, instead of `method`.
    pub batch: Option<Located<Vec<Located<BatchCall>>>>,
    /// Subscribe, collect notifications, unsubscribe (WebSocket only).
    pub subscription: Option<Located<SubscriptionSpec>>,
    /// Safe to send again: allows `settings.retry`.
    pub idempotent: Option<bool>,
    pub headers: Option<Vec<NameValue>>,
    pub auth: Option<Auth>,
    pub settings: Option<JsonrpcSettings>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

/// One call of a `batch`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchCall {
    pub method: Option<String>,
    pub params: Option<Located<Value>>,
    pub id: Option<Located<Value>>,
    pub notification: Option<bool>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
}

/// The generic subscription triple (design D4) and its headless bounds.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionSpec {
    pub subscribe: String,
    pub unsubscribe: String,
    pub notification: String,
    /// JSONPath of the subscription id inside a notification.
    pub notification_id_path: Option<String>,
    pub max_events: Option<u32>,
    pub max_duration_ms: Option<u64>,
}

impl SubscriptionSpec {
    pub fn effective_notification_id_path(&self) -> &str {
        self.notification_id_path
            .as_deref()
            .unwrap_or(DEFAULT_NOTIFICATION_ID_PATH)
    }

    pub fn effective_max_events(&self) -> u32 {
        self.max_events.unwrap_or(DEFAULT_SUBSCRIPTION_EVENTS)
    }

    pub fn effective_max_duration_ms(&self) -> u64 {
        self.max_duration_ms
            .unwrap_or(DEFAULT_SUBSCRIPTION_DURATION_MS)
    }
}

/// `settings.retry`: attempts after the first, and the first backoff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySettings {
    pub attempts: Option<u32>,
    pub backoff_ms: Option<u64>,
}

impl RetrySettings {
    pub fn effective_attempts(&self) -> u32 {
        self.attempts.unwrap_or(0)
    }

    pub fn effective_backoff_ms(&self) -> u64 {
        self.backoff_ms.unwrap_or(DEFAULT_RETRY_BACKOFF_MS)
    }
}

/// The settings of a `jsonrpc` file: those of `kind: http` (same meaning)
/// plus `retry` and `validate`. `Debug` hides the PKCS#12 password and
/// proxy credentials, as [`RequestSettings`] does.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonrpcSettings {
    pub timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    pub follow_redirects: Option<bool>,
    pub max_redirects: Option<u32>,
    pub verify_tls: Option<bool>,
    pub http2_prior_knowledge: Option<bool>,
    pub proxy: Option<String>,
    pub cookies: Option<bool>,
    pub decompress: Option<bool>,
    pub ca_bundle: Option<Located<String>>,
    pub client_cert: Option<Located<String>>,
    pub client_key: Option<Located<String>>,
    pub client_pkcs12: Option<Located<String>>,
    pub client_pkcs12_password: Option<String>,
    pub retry: Option<RetrySettings>,
    /// `false` skips validation against an OpenRPC catalog.
    pub validate: Option<bool>,
}

impl JsonrpcSettings {
    /// The transport settings, as a `kind: http` request has them.
    pub fn http(&self) -> RequestSettings {
        RequestSettings {
            timeout_ms: self.timeout_ms,
            connect_timeout_ms: self.connect_timeout_ms,
            follow_redirects: self.follow_redirects,
            max_redirects: self.max_redirects,
            verify_tls: self.verify_tls,
            http2_prior_knowledge: self.http2_prior_knowledge,
            proxy: self.proxy.clone(),
            cookies: self.cookies,
            decompress: self.decompress,
            ca_bundle: self.ca_bundle.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            client_pkcs12: self.client_pkcs12.clone(),
            client_pkcs12_password: self.client_pkcs12_password.clone(),
        }
    }
}

impl fmt::Debug for JsonrpcSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let password = self
            .client_pkcs12_password
            .as_ref()
            .map(|_| Hidden("client_pkcs12_password"));
        let proxy = self.proxy.as_ref().map(|_| placeholder("proxy"));
        f.debug_struct("JsonrpcSettings")
            .field(
                "http",
                &RequestSettings {
                    proxy,
                    ..self.http()
                },
            )
            .field("client_pkcs12_password", &password)
            .field("retry", &self.retry)
            .field("validate", &self.validate)
            .finish()
    }
}

/// The transport an endpoint selects, when its scheme is written out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointScheme {
    Http,
    WebSocket,
}

impl EndpointScheme {
    /// The scheme of `url`; `None` for other schemes and for URLs whose
    /// scheme comes from a variable.
    pub fn of(url: &str) -> Option<Self> {
        let lower = url.trim_start().to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            Some(Self::Http)
        } else if lower.starts_with("ws://") || lower.starts_with("wss://") {
            Some(Self::WebSocket)
        } else {
            None
        }
    }
}

/// One call as the file defines it, single or batched.
#[derive(Clone, Copy, Debug)]
pub struct CallFields<'a> {
    pub method: Option<&'a str>,
    pub params: Option<&'a Located<Value>>,
    pub id: Option<&'a Located<Value>>,
    pub notification: bool,
    pub assertions: &'a [Located<Assertion>],
    pub captures: &'a [Checked<Capture>],
}

impl BatchCall {
    pub fn fields(&self) -> CallFields<'_> {
        CallFields {
            method: self.method.as_deref(),
            params: self.params.as_ref(),
            id: self.id.as_ref(),
            notification: self.notification.unwrap_or(false),
            assertions: self.assertions.as_deref().unwrap_or_default(),
            captures: self.captures.as_deref().unwrap_or_default(),
        }
    }
}

impl JsonrpcRequest {
    /// Parse and check the `kind: jsonrpc` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        Checker { file, text }.request(&request)?;
        request.source = text.to_owned();
        Ok(request)
    }

    /// The single call (`None` for a batch file).
    pub fn call(&self) -> Option<CallFields<'_>> {
        if self.batch.is_some() {
            return None;
        }
        let subscribe = self.subscription.as_ref().map(|s| s.subscribe.as_str());
        Some(CallFields {
            method: self.method.as_deref().or(subscribe),
            params: self.params.as_ref(),
            id: self.id.as_ref(),
            notification: self.is_notification(),
            assertions: self.assertions.as_deref().unwrap_or_default(),
            captures: self.captures.as_deref().unwrap_or_default(),
        })
    }

    /// Every call, in file order: the single call, or the batch.
    pub fn calls(&self) -> Vec<CallFields<'_>> {
        match &self.batch {
            Some(batch) => batch.iter().map(|c| c.fields()).collect(),
            None => self.call().into_iter().collect(),
        }
    }

    pub fn is_notification(&self) -> bool {
        self.notification.unwrap_or(false)
    }

    pub fn is_idempotent(&self) -> bool {
        self.idempotent.unwrap_or(false)
    }

    /// `settings.retry`, or no retries.
    pub fn retry(&self) -> RetrySettings {
        self.settings
            .as_ref()
            .and_then(|s| s.retry)
            .unwrap_or_default()
    }

    /// The transport settings, as a `kind: http` request has them.
    pub fn http_settings(&self) -> Option<RequestSettings> {
        self.settings.as_ref().map(JsonrpcSettings::http)
    }

    /// The assertions of the request, in the shape the assertion engine
    /// takes.
    pub fn assertion_list(&self) -> Vec<Assertion> {
        unlocated(self.assertions.as_deref())
    }

    /// Every workspace file the request refers to, with its position.
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        let mut paths = Vec::new();
        if let Some(s) = &self.settings {
            let tls = [
                &s.ca_bundle,
                &s.client_cert,
                &s.client_key,
                &s.client_pkcs12,
            ];
            paths.extend(tls.into_iter().flatten());
        }
        for call in self.calls() {
            paths.extend(call.assertions.iter().filter_map(|a| match a.get() {
                Assertion::Schema(schema) => schema.file.as_ref(),
                _ => None,
            }));
        }
        paths
    }
}

/// `request` (the `kind: jsonrpc` file at `file`) as the HTTP POST that
/// sends it: its envelope, or an array for a batch, with explicit ids as
/// written and sequential ids (skipping the explicit ones) for the rest.
/// WebSocket endpoints and subscriptions have no HTTP form.
pub fn jsonrpc_as_http(file: &Path, request: &JsonrpcRequest) -> Result<HttpRequest, LoadError> {
    let websocket = EndpointScheme::of(&request.endpoint) == Some(EndpointScheme::WebSocket);
    if websocket || request.subscription.is_some() {
        let message = "jsonrpc: only calls to an http(s) endpoint are sent as an HTTP POST";
        return Err(LoadError::new(file, None, message));
    }
    let calls = request.calls();
    let taken: Vec<&Value> = calls
        .iter()
        .filter_map(|c| c.id.map(Located::get))
        .collect();
    let mut next = 0u64;
    let mut envelopes = Vec::with_capacity(calls.len());
    for call in &calls {
        let mut envelope = serde_json::Map::new();
        envelope.insert("jsonrpc".into(), Value::from("2.0"));
        envelope.insert(
            "method".into(),
            Value::from(call.method.unwrap_or_default()),
        );
        if let Some(params) = call.params {
            envelope.insert("params".into(), params.get().clone());
        }
        if !call.notification {
            let id = call
                .id
                .map_or_else(|| next_id(&mut next, &taken), |id| id.get().clone());
            envelope.insert("id".into(), id);
        }
        envelopes.push(Value::Object(envelope));
    }
    let body = if request.batch.is_some() {
        Value::Array(envelopes)
    } else {
        envelopes.into_iter().next().unwrap_or_default()
    };
    Ok(HttpRequest {
        version: super::kind::REQUEST_VERSION,
        kind: "http".into(),
        name: request.name.clone(),
        description: request.description.clone(),
        tags: request.tags.clone(),
        docs: request.docs.clone(),
        method: Some(Method::parse("POST").expect("POST is a method")),
        url: request.endpoint.clone(),
        query: None,
        headers: request.headers.clone(),
        body: Some(Body::Json(JsonContent::Value(body))),
        auth: request.auth.clone(),
        transform: None,
        settings: request.http_settings(),
        variables: request.variables.clone(),
        assertions: None,
        captures: None,
    })
}

/// The next sequential id that is not one of `taken`.
fn next_id(next: &mut u64, taken: &[&Value]) -> Value {
    loop {
        *next += 1;
        let id = Value::from(*next);
        if !taken.contains(&&id) {
            return id;
        }
    }
}

/// `assertions` without their positions.
pub fn unlocated(assertions: Option<&[Located<Assertion>]>) -> Vec<Assertion> {
    assertions
        .unwrap_or_default()
        .iter()
        .map(|a| a.get().clone())
        .collect()
}

/// The load-time checks of a `jsonrpc` file; every error names its line.
struct Checker<'a> {
    file: &'a Path,
    text: &'a str,
}

impl Checker<'_> {
    fn fail<T>(&self, at: Option<Position>, message: &str) -> Result<T, LoadError> {
        Err(LoadError::new(self.file, at, message))
    }

    fn request(&self, r: &JsonrpcRequest) -> Result<(), LoadError> {
        let scheme = EndpointScheme::of(&r.endpoint);
        self.endpoint(&r.endpoint)?;
        self.shape(r)?;
        if let Some(subscription) = &r.subscription {
            self.subscription(subscription, scheme)?;
        }
        if let Some(batch) = &r.batch {
            self.batch_size(batch)?;
        }
        let call_positions: Vec<Option<Position>> = match &r.batch {
            Some(batch) => batch.iter().map(Located::position).collect(),
            None => vec![self.key_line("method").or(Some(FIRST_LINE))],
        };
        for (call, at) in r.calls().into_iter().zip(call_positions) {
            self.call(&call, at, scheme)?;
        }
        self.unique_ids(&r.calls())
    }

    /// The position of the top-level `key:` of the file, when the file is
    /// YAML written one key per line.
    fn key_line(&self, key: &str) -> Option<Position> {
        let prefix = format!("{key}:");
        let index = self.text.lines().position(|l| l.starts_with(&prefix))?;
        Some(Position {
            line: index + 1,
            column: 1,
        })
    }

    /// Two calls of a batch with the same explicit id could not be told
    /// apart in the answer.
    fn unique_ids(&self, calls: &[CallFields<'_>]) -> Result<(), LoadError> {
        let mut seen: Vec<&Value> = Vec::new();
        for id in calls.iter().filter_map(|c| c.id) {
            if seen.contains(&id.get()) {
                let message = format!("duplicate id {} in batch", id.get());
                return self.fail(id.position(), &message);
            }
            seen.push(id.get());
        }
        Ok(())
    }

    fn endpoint(&self, endpoint: &str) -> Result<(), LoadError> {
        let trimmed = endpoint.trim();
        let at = self.key_line("endpoint").or(Some(FIRST_LINE));
        if trimmed.is_empty() {
            return self.fail(at, "jsonrpc: endpoint must not be empty");
        }
        let templated = trimmed.starts_with("{{");
        if !templated && EndpointScheme::of(trimmed).is_none() {
            return self.fail(
                at,
                "jsonrpc: endpoint must be an http, https, ws or wss URL",
            );
        }
        Ok(())
    }

    /// `method` and `batch` exclude each other; subscriptions are single
    /// calls that expect a result.
    fn shape(&self, r: &JsonrpcRequest) -> Result<(), LoadError> {
        if let Some(batch) = &r.batch {
            let single = r.method.is_some() || r.params.is_some() || r.id.is_some();
            if single || r.notification.is_some() {
                return self.fail(
                    batch.position(),
                    "a batch file sets method, params, id and notification per call",
                );
            }
        }
        let Some(subscription) = &r.subscription else {
            return Ok(());
        };
        if r.batch.is_some() || r.is_notification() {
            return self.fail(
                subscription.position(),
                "subscriptions cannot be batched or sent as notifications",
            );
        }
        Ok(())
    }

    fn subscription(
        &self,
        subscription: &Located<SubscriptionSpec>,
        scheme: Option<EndpointScheme>,
    ) -> Result<(), LoadError> {
        let at = subscription.position();
        if scheme == Some(EndpointScheme::Http) {
            return self.fail(at, "subscriptions require a ws:// or wss:// endpoint");
        }
        let names = [
            &subscription.subscribe,
            &subscription.unsubscribe,
            &subscription.notification,
        ];
        if names.iter().any(|n| n.trim().is_empty()) {
            return self.fail(at, "subscription method names must not be empty");
        }
        if subscription.max_events == Some(0) {
            return self.fail(at, "max_events must be at least 1");
        }
        Ok(())
    }

    fn batch_size(&self, batch: &Located<Vec<Located<BatchCall>>>) -> Result<(), LoadError> {
        match batch.len() {
            0 => self.fail(batch.position(), "batch must have at least 1 call"),
            n if n > MAX_BATCH_CALLS => self.fail(batch.position(), "batch exceeds 100 calls"),
            _ => Ok(()),
        }
    }

    fn call(
        &self,
        call: &CallFields<'_>,
        at: Option<Position>,
        scheme: Option<EndpointScheme>,
    ) -> Result<(), LoadError> {
        match call.method {
            Some(method) if !method.trim().is_empty() => {}
            _ => return self.fail(at, "jsonrpc request requires 'method'"),
        }
        if let Some(params) = call.params
            && !matches!(params.get(), Value::Array(_) | Value::Object(_))
        {
            return self.fail(params.position(), "params must be a list or a map");
        }
        if let Some(id) = call.id {
            self.id(id, call.notification)?;
        }
        for assertion in call.assertions {
            self.assertion(assertion, call.notification, scheme)?;
        }
        Ok(())
    }

    fn id(&self, id: &Located<Value>, notification: bool) -> Result<(), LoadError> {
        if notification {
            return self.fail(id.position(), "notifications have no id");
        }
        let valid = match id.get() {
            Value::String(_) => true,
            Value::Number(n) => n.is_i64() || n.is_u64(),
            _ => false,
        };
        if !valid {
            return self.fail(id.position(), "id must be a string or an integer");
        }
        Ok(())
    }

    fn assertion(
        &self,
        assertion: &Located<Assertion>,
        notification: bool,
        scheme: Option<EndpointScheme>,
    ) -> Result<(), LoadError> {
        let at = assertion.position();
        let ws = scheme == Some(EndpointScheme::WebSocket);
        match assertion.get() {
            Assertion::Status(_) if ws => {
                self.fail(at, "status assertions are not available over WebSocket")
            }
            Assertion::Header(_) if ws => {
                self.fail(at, "header assertions are not available over WebSocket")
            }
            Assertion::Json(json) if notification && addresses_result(&json.path) => {
                self.fail(at, "notifications have no result to assert on")
            }
            _ => Ok(()),
        }
    }
}

/// Line 1: the position of errors about the file as a whole.
const FIRST_LINE: Position = Position { line: 1, column: 1 };

/// Whether a JSONPath reads the `result` member of the envelope.
fn addresses_result(path: &str) -> bool {
    let path = path.trim();
    ["$.result", "$['result']", "$[\"result\"]"]
        .iter()
        .any(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<JsonrpcRequest, LoadError> {
        JsonrpcRequest::parse(Path::new("r.rman.yaml"), text)
    }

    const HEAD: &str = "version: 1\nkind: jsonrpc\nname: r\n";

    #[test]
    fn a_minimal_call_loads_with_defaults() {
        let r = parse(&format!(
            "{HEAD}endpoint: '{{{{rpc_url}}}}'\nmethod: eth_blockNumber\n"
        ))
        .unwrap();
        let call = r.call().unwrap();
        assert_eq!(call.method, Some("eth_blockNumber"));
        assert!(call.params.is_none() && call.id.is_none() && !call.notification);
        assert!(!r.is_idempotent());
        assert_eq!(r.retry().effective_attempts(), 0);
        assert_eq!(r.retry().effective_backoff_ms(), 500);
        assert_eq!(r.calls().len(), 1);
    }

    #[test]
    fn subscription_defaults() {
        let r = parse(&format!(
            "{HEAD}endpoint: ws://n\nmethod: eth_subscribe\nsubscription: {{subscribe: eth_subscribe, unsubscribe: eth_unsubscribe, notification: eth_subscription}}\n"
        ))
        .unwrap();
        let s = r.subscription.unwrap();
        assert_eq!(s.effective_max_events(), 10);
        assert_eq!(s.effective_max_duration_ms(), 30_000);
        assert_eq!(s.effective_notification_id_path(), "$.params.subscription");
    }

    #[test]
    fn result_paths_are_recognised() {
        for path in ["$.result", "$.result.number", "$.result[0]", "$['result']"] {
            assert!(addresses_result(path), "{path}");
        }
        for path in ["$.results", "$.error.code", "$"] {
            assert!(!addresses_result(path), "{path}");
        }
    }

    #[test]
    fn endpoints_need_a_known_scheme_unless_templated() {
        let err = parse(&format!("{HEAD}endpoint: ftp://x\nmethod: m\n")).unwrap_err();
        assert!(err.message.contains("http, https, ws or wss"), "{err}");
        assert!(parse(&format!("{HEAD}endpoint: ''\nmethod: m\n")).is_err());
        assert!(parse(&format!("{HEAD}endpoint: '{{{{u}}}}/rpc'\nmethod: m\n")).is_ok());
    }

    #[test]
    fn ids_are_strings_or_integers() {
        let err = parse(&format!("{HEAD}endpoint: http://x\nmethod: m\nid: 1.5\n")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:6: id must be a string or an integer"
        );
        let err = parse(&format!(
            "{HEAD}endpoint: http://x\nmethod: m\nnotification: true\nid: 1\n"
        ))
        .unwrap_err();
        assert!(err.message.contains("notifications have no id"), "{err}");
    }

    #[test]
    fn duplicate_batch_ids_are_refused_at_their_line() {
        let err = parse(&format!(
            "{HEAD}endpoint: http://x\nbatch:\n  - method: a\n    id: 1\n  - method: b\n    id: 1\n"
        ))
        .unwrap_err();
        assert_eq!(err.to_string(), "r.rman.yaml:9: duplicate id 1 in batch");
        let distinct = "batch:\n  - method: a\n    id: 1\n  - method: b\n    id: \"1\"\n";
        assert!(parse(&format!("{HEAD}endpoint: http://x\n{distinct}")).is_ok());
    }

    #[test]
    fn method_and_endpoint_errors_name_their_line() {
        let err = parse(&format!("{HEAD}endpoint: http://x\nmethod: \"\"\n")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:5: jsonrpc request requires 'method'"
        );
        let err = parse(&format!("{HEAD}endpoint: ftp://x\nmethod: m\n")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:4: jsonrpc: endpoint must be an http, https, ws or wss URL"
        );
        let err = parse(&format!("{HEAD}endpoint: http://x\n")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:1: jsonrpc request requires 'method'"
        );
    }

    #[test]
    fn calls_over_http_become_a_json_post() {
        let file = Path::new("r.rman.yaml");
        let single = parse(&format!(
            "{HEAD}endpoint: '{{{{rpc_url}}}}'\nmethod: getblockcount\nparams: ['{{{{h}}}}']\n"
        ))
        .unwrap();
        let http = jsonrpc_as_http(file, &single).unwrap();
        assert_eq!(http.effective_method(), "POST");
        assert_eq!(http.url, "{{rpc_url}}");
        let Some(Body::Json(JsonContent::Value(body))) = &http.body else {
            panic!("a JSON body");
        };
        assert_eq!(
            body,
            &serde_json::json!({"jsonrpc": "2.0", "method": "getblockcount", "params": ["{{h}}"], "id": 1})
        );
        let batch = parse(&format!(
            "{HEAD}endpoint: http://x\nbatch:\n  - method: a\n    id: 1\n  - method: b\n  - method: c\n    notification: true\n"
        ))
        .unwrap();
        let Some(Body::Json(JsonContent::Value(body))) =
            jsonrpc_as_http(file, &batch).unwrap().body
        else {
            panic!("a JSON body");
        };
        assert_eq!(
            body,
            serde_json::json!([
                {"jsonrpc": "2.0", "method": "a", "id": 1},
                {"jsonrpc": "2.0", "method": "b", "id": 2},
                {"jsonrpc": "2.0", "method": "c"}
            ])
        );
        let ws = parse(&format!("{HEAD}endpoint: ws://x\nmethod: m\n")).unwrap();
        assert!(jsonrpc_as_http(file, &ws).is_err());
    }

    #[test]
    fn settings_debug_hides_credentials() {
        let settings = JsonrpcSettings {
            proxy: Some("http://u:pw-x@p:1".into()),
            client_pkcs12_password: Some("pkcs-x".into()),
            ..JsonrpcSettings::default()
        };
        let debug = format!("{settings:?}");
        assert!(
            !debug.contains("pw-x") && !debug.contains("pkcs-x"),
            "{debug}"
        );
        assert_eq!(settings.http().proxy.as_deref(), Some("http://u:pw-x@p:1"));
    }
}
