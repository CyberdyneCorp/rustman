//! Running one probe: each step is resolved like any request (captures of
//! earlier steps, then the identity's environment and secrets), sent through
//! the [`GovernedSender`] (never the raw engine), and judged by the
//! detectors. Evidence is redacted with the exchange's own redactor, which
//! also knows every secret of both identities (a response can echo a secret
//! the step did not send), and each finding carries its [reproduction](super::repro). Resources that
//! steps create, and deletes that remove them, are reported for cleanup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rustman_core::collection::RequestEntry;
use rustman_core::model::{
    Auth, Body, HttpRequest, JsonContent, Method, NameValue, REQUEST_VERSION, Request, VarValue,
    Variables,
};
use rustman_core::settings::{HttpDefaults, Settings};
use rustman_core::trace::{TRACERESPONSE, TracePropagation, TraceRecord};
use rustman_core::vars::{
    EnvironmentSource, Layer, ResolvedBody, ResolvedRequest, Resolver, SecretLookup,
    VariablesSource, resolve_request,
};
use rustman_core::{Environment, Redactor, RequestRef, Secret};
use rustman_http::{Exchange, GovernedError, GovernedSender, SendOptions, export_curl};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::credentials::{
    add_credentials, add_plain_credentials, auth_headers, credential_header, keep_references,
    shown_credential,
};
use super::detect::{self, Detected, EVIDENCE_BYTES, Evidence, Finding, Observed, Scope};
use super::graph::Graph;
use super::plan::{AuthMode, Disposition, Family, Probe, Step, escape, segment};
use super::repro::{Regression, Reproduction, SecretReferences, request_file};
use super::resources::{Creator, Resource, creators};
use super::rules::rule;
use crate::collection::Security;
use crate::openapi::model::{ApiModel, Operation};
use rustman_report::exploration::EvidenceTrace;

/// Whose credentials and variables a step uses.
#[derive(Clone, Copy)]
pub struct Identity<'a> {
    pub environment: Option<&'a Environment>,
    pub secrets: &'a dyn SecretLookup,
}

impl std::fmt::Debug for Identity<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.environment.map(|e| e.name.as_str());
        f.debug_struct("Identity")
            .field("environment", &name)
            .finish_non_exhaustive()
    }
}

/// Everything probes run against.
#[derive(Clone, Debug)]
pub struct Context<'a> {
    pub model: &'a ApiModel,
    /// The resource graph, to identify what create steps made.
    pub graph: &'a Graph,
    /// The base URL every step path is appended to.
    pub target: &'a str,
    /// `rustman.toml [variables]`.
    pub variables: &'a Variables,
    pub identity: Identity<'a>,
    /// `--identity-b`.
    pub identity_b: Option<Identity<'a>>,
    /// The enabled scan families.
    pub scans: &'a [Family],
    /// `[explore].disabled_rules`.
    pub disabled_rules: &'a [String],
    pub send: SendOptions,
}

/// The transport options of an exploration: the defaults, with the trace
/// propagation of `environment` over `settings` (under `scope: run`, one
/// trace id for every probe of the exploration).
pub fn exploration_send_options(
    settings: &Settings,
    environment: Option<&Environment>,
    secrets: &dyn SecretLookup,
) -> Result<SendOptions, String> {
    let none = Variables::default();
    let trace = TracePropagation::for_workspace(settings, environment, secrets, &none, false)?;
    Ok(SendOptions {
        trace: trace.map(|(propagation, _)| propagation),
        ..SendOptions::default()
    })
}

/// The warning shown when `[trace.export]` is on for an exploration:
/// probes carry trace context, but their client spans are not exported.
pub const EXPORT_NOT_SUPPORTED: &str =
    "trace.export: explorations do not export client spans yet; probes still carry trace headers";

/// [`EXPORT_NOT_SUPPORTED`] when the active environment's trace
/// configuration over `settings` enables export.
pub fn exploration_export_warning(
    settings: &Settings,
    environment: Option<&Environment>,
) -> Option<&'static str> {
    let config = match environment {
        Some(env) => env.trace.over(&settings.trace),
        None => settings.trace.clone(),
    };
    config.is_exporting().then_some(EXPORT_NOT_SUPPORTED)
}

/// How a probe ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "reason")]
pub enum Status {
    /// Every step ran.
    Done,
    /// The policy refused a step (`blocked: <reason>`).
    Blocked(String),
    /// A budget ran out (`not run: budget`).
    NotRun(String),
    Skipped(String),
    /// A step could not be sent, or a chain stopped early.
    Failed(String),
}

/// The reason of a probe cut short by a cancellation.
const INTERRUPTED: &str = "interrupted";

impl Status {
    /// A probe still in flight when the drain time after a cancellation
    /// ran out.
    pub fn interrupted() -> Self {
        Self::NotRun(INTERRUPTED.into())
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::NotRun(reason) if reason == INTERRUPTED)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done => f.write_str("done"),
            Self::Blocked(reason) => write!(f, "blocked: {reason}"),
            Self::NotRun(reason) => write!(f, "not run: {reason}"),
            Self::Skipped(reason) => write!(f, "skipped: {reason}"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
        }
    }
}

/// One request a probe sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub method: String,
    /// Redacted.
    pub url: String,
    pub status: u16,
}

/// What running a probe produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub probe: String,
    pub status: Status,
    pub sent: Vec<Sent>,
    pub findings: Vec<Finding>,
    /// Resources its steps created, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<Resource>,
    /// Paths (templates, like [`Resource::path`]) its steps deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
}

impl Outcome {
    /// An outcome of `probe` that sent nothing.
    pub fn new(probe: &str, status: Status) -> Self {
        Self {
            probe: probe.to_owned(),
            status,
            sent: Vec::new(),
            findings: Vec::new(),
            created: Vec::new(),
            deleted: Vec::new(),
        }
    }
}

/// Runs probes through one governed sender.
pub struct Executor<'a> {
    sender: &'a GovernedSender,
    context: Context<'a>,
    security: Security,
    creators: Vec<Creator>,
    /// Every secret of identity A and identity B, and those the trace
    /// headers carry.
    known: Redactor,
    /// What each probe in flight has created so far.
    in_flight: Mutex<BTreeMap<String, Vec<Resource>>>,
}

/// One step's response, with what it was sent as.
struct Seen<'r> {
    exchange: &'r Exchange,
    resolved: &'r ResolvedRequest,
    redactor: &'r Redactor,
}

/// The state a chain carries from step to step.
#[derive(Default)]
struct Chain {
    captures: Vec<(String, VarValue)>,
    previous: Option<Vec<u8>>,
}

impl<'a> Executor<'a> {
    pub fn new(sender: &'a GovernedSender, context: Context<'a>) -> Self {
        let security = Security::of(context.model, &mut Vec::new());
        let creators = creators(context.model, context.graph);
        let identities = || std::iter::once(context.identity).chain(context.identity_b);
        let mut known = identity_secrets(identities());
        add_plain_credentials(&mut known, identities(), &security.secrets);
        for secret in context.send.trace.iter().flat_map(|t| &t.secrets) {
            known.add(secret);
        }
        Self {
            sender,
            context,
            security,
            creators,
            known,
            in_flight: Mutex::default(),
        }
    }

    /// The sender every request goes through.
    pub fn sender(&self) -> &GovernedSender {
        self.sender
    }

    /// Take what the probes still in flight (whose futures were dropped)
    /// have created: `(probe, resources)`, the probes that created nothing
    /// left out.
    pub fn abandoned(&self) -> Vec<(String, Vec<Resource>)> {
        std::mem::take(&mut *self.lock_in_flight())
            .into_iter()
            .filter(|(_, created)| !created.is_empty())
            .collect()
    }

    fn lock_in_flight(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<Resource>>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `probe`. A blocked or skipped probe sends nothing.
    pub async fn run(&self, probe: &Probe) -> Outcome {
        let mut outcome = Outcome::new(&probe.id, Status::Done);
        match &probe.disposition {
            Disposition::Allowed => {}
            Disposition::Blocked(reason) => {
                outcome.status = Status::Blocked(reason.clone());
                return outcome;
            }
            Disposition::Skipped(reason) => {
                outcome.status = Status::Skipped(reason.clone());
                return outcome;
            }
        }
        self.lock_in_flight().insert(probe.id.clone(), Vec::new());
        let mut chain = Chain::default();
        for step in &probe.steps {
            if let Err(status) = self.step(probe, step, &mut chain, &mut outcome).await {
                outcome.status = status;
                break;
            }
        }
        self.lock_in_flight().remove(&probe.id);
        outcome
    }

    async fn step(
        &self,
        probe: &Probe,
        step: &Step,
        chain: &mut Chain,
        outcome: &mut Outcome,
    ) -> Result<(), Status> {
        let operation = step.operation.as_deref().and_then(|k| self.operation(k));
        let mut sent = step.clone();
        sent.path = inline_path(&step.path, &chain.captures);
        let request = self.request(&probe.id, &sent, self.auth_for(step.auth, operation));
        let resolved = self.resolve(request, step.auth, chain.captures.clone())?;
        let exchange = self.send(&resolved).await?;
        let mut redactor = exchange.redactor(&resolved);
        redactor.merge(&self.known);
        add_credentials(&mut redactor, &resolved);
        outcome.sent.push(Sent {
            step: step.name.clone(),
            operation: step.operation.clone(),
            method: step.method.clone(),
            url: redactor.text(&exchange.url).into_owned(),
            status: exchange.status,
        });
        let seen = Seen {
            exchange: &exchange,
            resolved: &resolved,
            redactor: &redactor,
        };
        self.findings(probe, step, operation, chain, &seen, outcome);
        self.track(&probe.id, step, &chain.captures, &seen, outcome);
        let captured = capture(step, &exchange);
        let complete = captured.len() == step.captures.len();
        chain.captures.extend(captured);
        chain.previous = Some(exchange.body.to_vec());
        let success = (200..300).contains(&exchange.status);
        if step.require_success && !(success && complete) {
            return Err(Status::Failed(format!(
                "{} {} answered {}{}",
                step.name,
                step.method,
                exchange.status,
                if complete {
                    ""
                } else {
                    " without the captured values"
                }
            )));
        }
        Ok(())
    }

    async fn send(&self, resolved: &ResolvedRequest) -> Result<Exchange, Status> {
        let sent = self.sender.send(resolved, &self.context.send).await;
        sent.map_err(status_of)
    }

    /// Delete `resource` with its documented delete operation, as identity
    /// A, and return the status it answered.
    pub async fn delete(&self, resource: &Resource) -> Result<u16, Status> {
        let key = resource
            .delete
            .as_deref()
            .ok_or_else(|| Status::Skipped("no delete operation".into()))?;
        let step = Step {
            name: "cleanup".into(),
            operation: Some(key.to_owned()),
            method: "DELETE".into(),
            path: resource.path.clone(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            auth: AuthMode::Identity,
            check: None,
            captures: Vec::new(),
            require_success: false,
            creates: false,
        };
        let auth = self.auth_for(step.auth, self.operation(key));
        let request = self.request("cleanup", &step, auth);
        let resolved = self.resolve(request, step.auth, Vec::new())?;
        // Cleanup is not budgeted: the budgets have usually run out by now.
        let sent = self
            .sender
            .send_cleanup(&resolved, &self.context.send)
            .await;
        Ok(sent.map_err(status_of)?.status)
    }

    /// Record the findings of a step's response, each with its evidence
    /// and reproduction.
    fn findings(
        &self,
        probe: &Probe,
        step: &Step,
        operation: Option<&Operation>,
        chain: &Chain,
        seen: &Seen<'_>,
        outcome: &mut Outcome,
    ) {
        let found = self.detect(step, operation, seen.exchange, chain.previous.as_deref());
        if found.is_empty() {
            return;
        }
        let inlined = inline_step(step, &chain.captures);
        let template = self.request(&probe.id, &inlined, self.auth_for(step.auth, operation));
        let curl = seen
            .redactor
            .text(&self.curl(&template, step.auth))
            .into_owned();
        let target = escape(self.context.target);
        for detected in found {
            let evidence = evidence(&step.method, seen);
            let mut finding = finding(probe, step, detected, seen.redactor, evidence);
            let regression = Regression {
                rule: &finding.rule,
                operation,
                components: &self.context.model.components,
                status: seen.exchange.status,
                check: step.check.as_ref(),
                message: &finding.message,
            };
            let request = request_file(template.clone(), &target, &regression);
            finding.reproduction = Some(Reproduction {
                curl: curl.clone(),
                request: seen.redactor.text(&request).into_owned(),
            });
            outcome.findings.push(finding);
        }
    }

    /// Record what a successful step created or deleted.
    fn track(
        &self,
        probe: &str,
        step: &Step,
        captures: &[(String, VarValue)],
        seen: &Seen<'_>,
        outcome: &mut Outcome,
    ) {
        if !(200..300).contains(&seen.exchange.status) {
            return;
        }
        let path = inline_path(&step.path, captures);
        if step.method == "DELETE" {
            outcome.deleted.push(path);
            return;
        }
        let creator = step
            .operation
            .as_deref()
            .filter(|_| step.creates)
            .and_then(|key| self.creators.iter().find(|c| c.create == key));
        if let Some(creator) = creator {
            let url = seen.redactor.text(&seen.exchange.url);
            let body = &seen.exchange.body;
            let created = creator.resource(probe, &path, &url, body);
            if let Some(slot) = self.lock_in_flight().get_mut(probe) {
                slot.extend(created.iter().cloned());
            }
            outcome.created.extend(created);
        }
    }

    /// `template` as a curl command, with secrets and every credential (a
    /// plain variable's too) as `{{name}}` references.
    fn curl(&self, template: &HttpRequest, auth: AuthMode) -> String {
        let environment = self.identity(auth).and_then(|i| i.environment);
        let template = keep_references(template.clone());
        self.resolve_with(template, environment, &SecretReferences, Vec::new())
            .map(|r| export_curl(&r, true).command)
            .unwrap_or_default()
    }

    fn operation(&self, key: &str) -> Option<&'a Operation> {
        self.context
            .model
            .operations
            .iter()
            .find(|op| super::operation_key(op) == key)
    }

    fn detect(
        &self,
        step: &Step,
        operation: Option<&Operation>,
        exchange: &Exchange,
        previous: Option<&[u8]>,
    ) -> Vec<Detected> {
        let scope = Scope {
            operation,
            components: &self.context.model.components,
            scans: self.context.scans,
            previous,
        };
        let observed = Observed {
            status: exchange.status,
            headers: &exchange.headers,
            body: &exchange.body,
            https: exchange.url.starts_with("https:"),
        };
        let disabled = self.context.disabled_rules;
        detect::detect(step.check.as_ref(), &scope, &observed)
            .into_iter()
            .filter(|d| !disabled.iter().any(|r| r == d.rule))
            .collect()
    }

    /// Whose credentials `mode` sends; `None` without a second identity.
    fn identity(&self, mode: AuthMode) -> Option<Identity<'a>> {
        match mode {
            AuthMode::None | AuthMode::Identity => Some(self.context.identity),
            AuthMode::IdentityB => self.context.identity_b,
        }
    }

    /// The auth a step in `mode` sends for `operation`.
    fn auth_for(&self, mode: AuthMode, operation: Option<&Operation>) -> Option<Auth> {
        match mode {
            AuthMode::None => Some(Auth::None),
            AuthMode::Identity | AuthMode::IdentityB => self.auth(operation),
        }
    }

    /// `request`, resolved for the identity of `mode` with `captures`.
    fn resolve(
        &self,
        request: HttpRequest,
        mode: AuthMode,
        captures: Vec<(String, VarValue)>,
    ) -> Result<ResolvedRequest, Status> {
        let identity = self
            .identity(mode)
            .ok_or_else(|| Status::Skipped("no second identity (--identity-b)".into()))?;
        self.resolve_with(request, identity.environment, identity.secrets, captures)
    }

    fn resolve_with(
        &self,
        request: HttpRequest,
        environment: Option<&Environment>,
        secrets: &dyn SecretLookup,
        captures: Vec<(String, VarValue)>,
    ) -> Result<ResolvedRequest, Status> {
        let entry = RequestEntry {
            path: PathBuf::from(format!("{}.rman.yaml", request.name)),
            request: Request::Http(Box::new(request)),
        };
        let http = HttpDefaults::default();
        let request = RequestRef {
            root: Path::new("."),
            http: &http,
            entry: &entry,
            ancestors: Vec::new(),
        };
        let mut resolver = Resolver::new();
        resolver.push(VariablesSource::owned(
            Layer::Capture,
            Variables::new(captures),
        ));
        if let Some(env) = environment {
            resolver.push(EnvironmentSource::new(env, secrets));
        }
        resolver.push(VariablesSource::new(
            Layer::Settings,
            self.context.variables,
        ));
        resolve_request(&request, &resolver).map_err(|e| Status::Failed(e.to_string()))
    }

    /// The auth of `operation` (the collection's for an undocumented method);
    /// `type: none` when it is public.
    fn auth(&self, operation: Option<&Operation>) -> Option<Auth> {
        let auth = match operation {
            Some(op) => self.security.effective(op),
            None => self.security.collection.clone(),
        };
        Some(auth.unwrap_or(Auth::None))
    }

    fn request(&self, name: &str, step: &Step, auth: Option<Auth>) -> HttpRequest {
        let rows = |pairs: &[(String, String)]| {
            let rows: Vec<NameValue> = pairs
                .iter()
                .map(|(n, v)| NameValue::new(escape(n), v))
                .collect();
            (!rows.is_empty()).then_some(rows)
        };
        HttpRequest {
            version: REQUEST_VERSION,
            kind: "http".into(),
            name: name.to_owned(),
            description: None,
            tags: None,
            docs: None,
            method: Method::parse(&step.method).ok(),
            url: format!(
                "{}{}",
                escape(self.context.target.trim_end_matches('/')),
                step.path
            ),
            query: rows(&step.query),
            headers: rows(&step.headers),
            body: step.body.clone().map(|v| Body::Json(JsonContent::Value(v))),
            auth,
            settings: None,
            variables: None,
            assertions: None,
            captures: None,
        }
    }
}

/// How a request the sender did not send ends its probe.
fn status_of(error: GovernedError) -> Status {
    match error {
        GovernedError::Blocked { reason, .. } => Status::Blocked(reason.reason().to_owned()),
        GovernedError::NotRun(_) => Status::NotRun("budget".into()),
        GovernedError::Request(e) => Status::Failed(e.to_string()),
    }
}

/// The values `step` captures from `exchange`'s JSON body.
fn capture(step: &Step, exchange: &Exchange) -> Vec<(String, VarValue)> {
    if step.captures.is_empty() {
        return Vec::new();
    }
    let Ok(body) = serde_json::from_slice::<Value>(&exchange.body) else {
        return Vec::new();
    };
    step.captures
        .iter()
        .filter_map(|c| {
            let value = match body.pointer(&c.pointer)? {
                Value::String(s) => s.clone(),
                Value::Null | Value::Array(_) | Value::Object(_) => return None,
                other => other.to_string(),
            };
            Some((c.variable.clone(), VarValue::String(value)))
        })
        .collect()
}

/// A finding of `step`'s response, filed under the step's operation (a
/// setup step's response is about that step's operation, not the probe's).
fn finding(
    probe: &Probe,
    step: &Step,
    detected: Detected,
    redactor: &Redactor,
    evidence: Evidence,
) -> Finding {
    let severity = rule(detected.rule).map_or(super::rules::Severity::Info, |r| r.severity);
    let (instance_pointer, schema_pointer) = detected.pointers.unzip();
    let operation = step.operation.as_ref().unwrap_or(&probe.operation);
    Finding {
        rule: detected.rule.to_owned(),
        severity,
        operation: operation.clone(),
        probe: probe.id.clone(),
        message: redactor.text(&detected.message).into_owned(),
        instance_pointer,
        schema_pointer,
        evidence,
        reproduction: None,
    }
}

/// The redacted exchange, with the body cut at [`EVIDENCE_BYTES`].
fn evidence(method: &str, seen: &Seen<'_>) -> Evidence {
    let (exchange, request, redactor) = (seen.exchange, seen.resolved, seen.redactor);
    let auth = request.auth.as_ref();
    let request_headers = request
        .headers
        .iter()
        .map(|h| {
            let value = redactor.resolved(&h.value);
            let value = match credential_header(&h.name.text, auth) {
                Some(name) => shown_credential(&name, &value),
                None => value,
            };
            (redactor.resolved(&h.name), value)
        })
        .chain(auth_headers(auth, redactor))
        .collect();
    let request_body = match &request.body {
        ResolvedBody::Json(value) => Some(excerpt(value.to_string().as_bytes(), redactor).0),
        ResolvedBody::JsonText(text) | ResolvedBody::Text(text) => {
            Some(excerpt(text.text.as_bytes(), redactor).0)
        }
        _ => None,
    };
    let response_headers = exchange
        .headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes());
            (
                name.to_string(),
                redactor.header_value(name.as_str(), &value),
            )
        })
        .collect();
    let (body, truncated) = if exchange.binary {
        (
            format!("<{} bytes of binary data>", exchange.size_bytes),
            false,
        )
    } else {
        excerpt(&exchange.body, redactor)
    };
    Evidence {
        method: method.to_owned(),
        url: redactor.text(&exchange.url).into_owned(),
        request_headers,
        request_body,
        status: exchange.status,
        response_headers,
        body,
        truncated,
        trace: evidence_trace(exchange),
    }
}

/// The trace ids an evidence exchange carried, with the backend's own.
fn evidence_trace(exchange: &Exchange) -> Option<EvidenceTrace> {
    let sent = exchange.trace.as_ref()?;
    let response = exchange
        .headers
        .get(TRACERESPONSE)
        .and_then(|v| v.to_str().ok());
    let record = TraceRecord::new(sent, response, (0, 0), &Redactor::new());
    Some(EvidenceTrace {
        trace_id: record.trace_id,
        span_id: record.span_id,
        sampled: record.sampled,
        backend_trace_id: record.backend_trace_id,
    })
}

/// A redactor of every secret the identities' environments declare and
/// the store holds.
fn identity_secrets<'a>(identities: impl Iterator<Item = Identity<'a>>) -> Redactor {
    let mut redactor = Redactor::new();
    for identity in identities {
        let Some(env) = identity.environment else {
            continue;
        };
        for name in &env.secrets {
            if let Ok(Some(value)) = identity.secrets.secret(&env.name, name) {
                redactor.add(&Secret::new(name.clone(), value));
            }
        }
    }
    redactor
}

/// The first [`EVIDENCE_BYTES`] of `bytes`, redacted, and whether it was cut.
fn excerpt(bytes: &[u8], redactor: &Redactor) -> (String, bool) {
    let kept = redactor.bytes_prefix(bytes, EVIDENCE_BYTES);
    let text = String::from_utf8_lossy(&kept).into_owned();
    (text, bytes.len() > EVIDENCE_BYTES)
}

/// `{{name}}` of each capture replaced by its value, escaped as a literal.
fn inline(template: &str, captures: &[(String, VarValue)]) -> String {
    inline_with(template, captures, str::to_owned)
}

/// [`inline`] for a URL path: each value is percent-encoded as one path
/// segment, so a captured `../admin` cannot move the request elsewhere.
fn inline_path(template: &str, captures: &[(String, VarValue)]) -> String {
    inline_with(template, captures, segment)
}

fn inline_with(
    template: &str,
    captures: &[(String, VarValue)],
    encode: impl Fn(&str) -> String,
) -> String {
    captures
        .iter()
        .fold(template.to_owned(), |text, (name, value)| {
            let VarValue::String(value) = value else {
                return text;
            };
            text.replace(&format!("{{{{{name}}}}}"), &escape(&encode(value)))
        })
}

/// `step` with the values captured so far written in, so it can be sent on
/// its own.
fn inline_step(step: &Step, captures: &[(String, VarValue)]) -> Step {
    let pairs = |pairs: &[(String, String)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.clone(), inline(v, captures)))
            .collect()
    };
    let mut inlined = step.clone();
    inlined.path = inline_path(&step.path, captures);
    inlined.query = pairs(&step.query);
    inlined.headers = pairs(&step.headers);
    inlined.body = step.body.as_ref().map(|b| inline_json(b, captures));
    inlined
}

fn inline_json(value: &Value, captures: &[(String, VarValue)]) -> Value {
    match value {
        Value::String(s) => Value::String(inline(s, captures)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| inline_json(v, captures)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), inline_json(v, captures)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_are_inlined_as_literals() {
        let captures = vec![
            ("id".to_owned(), VarValue::String("42".into())),
            ("odd".to_owned(), VarValue::String("{{x}}".into())),
        ];
        assert_eq!(inline("/users/{{id}}", &captures), "/users/42");
        assert_eq!(inline("{{odd}}", &captures), "\\{{x}}");
        let hostile = vec![
            ("id".to_owned(), VarValue::String("../admin".into())),
            ("up".to_owned(), VarValue::String("..".into())),
        ];
        assert_eq!(
            inline_path("/users/{{id}}/{{up}}", &hostile),
            "/users/..%2Fadmin/%2E%2E"
        );
        let value = serde_json::json!({"a": ["{{id}}", 1]});
        assert_eq!(
            inline_json(&value, &captures),
            serde_json::json!({"a": ["42", 1]})
        );
    }

    #[test]
    fn an_exploration_shares_the_run_trace_of_its_environment() {
        let settings = Settings::ad_hoc();
        let off = exploration_send_options(&settings, None, &rustman_core::vars::NoSecrets);
        assert!(off.unwrap().trace.is_none());
        let env = Environment {
            name: "staging".into(),
            trace: rustman_core::trace::TraceConfig {
                enabled: Some(true),
                scope: Some(rustman_core::trace::TraceScope::Run),
                ..Default::default()
            },
            ..Environment::default()
        };
        let on = exploration_send_options(&settings, Some(&env), &rustman_core::vars::NoSecrets);
        let trace = on.unwrap().trace.expect("propagation on");
        assert!(trace.run_trace_id.is_some());
    }

    #[test]
    fn an_exporting_exploration_is_warned_about() {
        let settings = Settings::ad_hoc();
        assert_eq!(exploration_export_warning(&settings, None), None);
        let env = Environment {
            name: "staging".into(),
            trace: rustman_core::trace::TraceConfig {
                export: Some(rustman_core::trace::ExportConfig {
                    enabled: true,
                    endpoint: Some("http://localhost:4317".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Environment::default()
        };
        assert_eq!(
            exploration_export_warning(&settings, Some(&env)),
            Some(EXPORT_NOT_SUPPORTED)
        );
    }
}
