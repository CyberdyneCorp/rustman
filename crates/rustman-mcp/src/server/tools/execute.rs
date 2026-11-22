//! Request execution tools (spec "Request execution tools"): `send_request`
//! and `run_collection`. Both go through the engine's runner (variables,
//! auth, assertions, captures, history) with an [`AgentGate`] in place of
//! the plain engine, so every request passes the agent permissions, the
//! human's confirmation and the session's `GovernedSender`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use rustman_core::collection::RequestEntry;
use rustman_core::history::HistoryStore;
use rustman_core::model::{EvmRequest, REQUEST_FILE_SUFFIX, Request, Variables};
use rustman_core::run::{Filter, RequestRecord, RequestStatus, plan};
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::{ResolvedRequest, parse_overrides};
use rustman_core::workspace::RequestRef;
use rustman_http::runner::{RunContext, RunOptions, Runner, SendGate, Sent};
use rustman_http::{Exchange, RequestError, SendOptions, import_curl_words};
use rustman_report::{Report, RunReport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::messaging::AgentMessaging;
use super::{Call, output};
use crate::server::confirm::Confirm;
use crate::server::context::{AgentContext, JobCaller, Target, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::{Job, JobKind, JobState, on_own_thread};
use crate::server::limits::BodyView;

/// The file name ad-hoc requests are reported under.
const AD_HOC: &str = "ad-hoc";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendRequestArgs {
    /// A saved request: its id (`users/get-user`) or file. Give either
    /// `path` or `url`.
    pub path: Option<String>,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
    /// The method of an ad-hoc request (default GET).
    pub method: Option<String>,
    /// The URL of an ad-hoc request; `{{variables}}` resolve in the
    /// session's environment.
    pub url: Option<String>,
    /// Headers of an ad-hoc request.
    pub headers: Option<BTreeMap<String, String>>,
    /// The body of an ad-hoc request, sent as is.
    pub body: Option<String>,
    /// Total timeout in milliseconds, replacing the request's own.
    pub timeout_ms: Option<u64>,
}

/// A header as received, redacted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HeaderView {
    pub name: String,
    pub value: String,
}

/// Where the time went, in milliseconds.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Timings {
    pub total_ms: f64,
    pub ttfb_ms: f64,
    pub download_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_ms: Option<f64>,
}

/// One assertion of a saved request.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AssertionView {
    pub passed: bool,
    /// What was checked and, when it failed, why.
    pub text: String,
}

/// The redacted exchange of `send_request`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SendResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `passed`, `failed` (an assertion), `errored` (no response) or
    /// `skipped`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The URL as sent, redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Why there is no response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub headers: Vec<HeaderView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    pub assertions: Vec<AssertionView>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunCollectionArgs {
    /// A folder or request file, relative to the workspace root (default:
    /// the whole workspace).
    pub path: Option<String>,
    /// Only matching requests: `tag:<name>` or a glob over request ids.
    #[serde(default)]
    pub filter: Vec<String>,
    /// Variables for this run, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
    /// Skip the remaining requests after the first failed or errored one.
    #[serde(default)]
    pub bail: bool,
}

/// A background job that started.
#[derive(Debug, Serialize, JsonSchema)]
pub struct JobStarted {
    pub job_id: String,
    /// How many requests the run executes.
    pub requests: usize,
}

/// The [`SendGate`] of agent sessions: [`AgentContext::send`] for every
/// request, remembering the first refusal so the tool can return its code.
pub struct AgentGate<'a, C: Confirm> {
    ctx: &'a AgentContext,
    confirm: &'a C,
    target: &'a Target,
    refused: Mutex<Option<ToolError>>,
    /// The background job sending, whose requests get audit lines of
    /// their own.
    job: Option<JobCaller>,
}

impl<'a, C: Confirm> AgentGate<'a, C> {
    pub fn new(ctx: &'a AgentContext, confirm: &'a C, target: &'a Target) -> Self {
        Self {
            ctx,
            confirm,
            target,
            refused: Mutex::new(None),
            job: None,
        }
    }

    /// A gate for the requests of a background job: each one gets an audit
    /// line naming `job`.
    pub fn for_job(
        ctx: &'a AgentContext,
        confirm: &'a C,
        target: &'a Target,
        job: JobCaller,
    ) -> Self {
        Self {
            job: Some(job),
            ..Self::new(ctx, confirm, target)
        }
    }

    /// Before a job sends `request`: refused unless the audit log can be
    /// written.
    fn audit_ready(&self) -> Result<(), ToolError> {
        match &self.job {
            Some(_) => self.ctx.audit_ready(),
            None => Ok(()),
        }
    }

    /// Audit `request` when a job sent it. A line that cannot be written
    /// fails the request.
    fn audit(&self, request: &ResolvedRequest, error: Option<&ToolError>) -> Result<(), ToolError> {
        match &self.job {
            Some(job) => self
                .ctx
                .audit_job_request(job, &request.method, &request.url.text, error),
            None => Ok(()),
        }
    }

    /// The first request the agent permissions, the policy or the human
    /// refused.
    pub fn refused(&self) -> Option<ToolError> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn refuse(&self, err: &ToolError) {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        refused.get_or_insert_with(|| err.clone());
    }
}

/// Whether `code` means the request was refused rather than failed.
fn is_refusal(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::Blocked | ErrorCode::Declined | ErrorCode::ConfirmationUnavailable
    )
}

impl<C: Confirm> SendGate for AgentGate<'_, C> {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            if let Err(err) = self.audit_ready() {
                self.refuse(&err);
                return Err(RequestError::Blocked(err.message));
            }
            let sent = self
                .ctx
                .send(request, options, self.confirm, self.target)
                .await;
            let audited = self.audit(request, sent.as_ref().err());
            sent.and_then(|exchange| audited.map(|()| exchange))
                .map_err(|err| self.request_error(err))
        })
    }

    /// A plugin kind request: the agent permissions and the human admit
    /// it ([`AgentContext::authorize_plugin_kind`]), and it is audited like
    /// any request (the job's own line, or the call's method and URL).
    fn authorize_kind<'a>(
        &'a self,
        request: &'a ResolvedRequest,
    ) -> BoxFuture<'a, Result<(), RequestError>> {
        Box::pin(async move {
            if let Err(err) = self.audit_ready() {
                self.refuse(&err);
                return Err(RequestError::Blocked(err.message));
            }
            let admitted = self
                .ctx
                .authorize_plugin_kind(request, self.confirm, self.target)
                .await;
            let audited = self.audit(request, admitted.as_ref().err());
            admitted.and(audited).map_err(|err| self.request_error(err))
        })
    }
}

impl<C: Confirm> AgentGate<'_, C> {
    /// The runner's error for `err`, remembering a refusal.
    fn request_error(&self, err: ToolError) -> RequestError {
        if is_refusal(err.code) {
            self.refuse(&err);
            RequestError::Blocked(err.message)
        } else {
            RequestError::Network(err.message)
        }
    }
}

pub(super) fn overrides(vars: Option<&BTreeMap<String, String>>) -> Result<Variables, ToolError> {
    let pairs: Vec<String> = vars
        .into_iter()
        .flatten()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    parse_overrides(&pairs).map_err(|e| ToolError::new(ErrorCode::InvalidInput, e))
}

/// The ad-hoc request of `args`, read the way `rustman send` reads it.
fn ad_hoc_entry(args: &SendRequestArgs, url: &str) -> Result<RequestEntry, ToolError> {
    if url.trim_start().starts_with('-') {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            format!("'{url}' is not a URL"),
        ));
    }
    let method = args.method.as_deref().unwrap_or("GET").to_ascii_uppercase();
    let mut words = vec!["-X".to_owned(), method, url.to_owned()];
    for (name, value) in args.headers.iter().flatten() {
        words.extend(["-H".to_owned(), format!("{name}: {value}")]);
    }
    if let Some(body) = &args.body {
        words.extend(["--data-raw".to_owned(), body.clone()]);
    }
    let imported = import_curl_words(&words)
        .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e.to_string()))?;
    let mut request = imported.request;
    request.name = AD_HOC.to_owned();
    Ok(RequestEntry {
        path: PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}")),
        request: Request::Http(Box::new(request)),
    })
}

pub(super) fn history(ctx: &AgentContext) -> Option<HistoryStore> {
    let ws = ctx.workspace();
    ws.settings
        .history
        .enabled
        .then(|| HistoryStore::for_workspace(&ws.root, &ws.settings.history))
}

/// Run `send` on a runner of the session with `options` and `gate`.
async fn send_one(
    call: &Call,
    target: &RequestRef<'_>,
    mut options: RunOptions<'_>,
) -> Result<Sent, ToolError> {
    let ctx = &call.ctx;
    let gate = AgentGate::new(ctx, &call.peer, &call.target);
    let messaging = AgentMessaging::new(ctx, &call.peer, &call.target);
    let environment = ctx.environment();
    let ws = ctx.workspace();
    options.history = history(ctx);
    options.gate = Some(&gate);
    options.plugins = ctx.plugins();
    options.messaging = Some(&messaging);
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
    match gate.refused().or_else(|| messaging.refused()) {
        Some(err) => Err(err),
        None => Ok(sent),
    }
}

/// Send on a thread of the session's own: the runner's futures are not
/// `Send`.
pub async fn send_request(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { send_here(&call).await }).await?
}

async fn send_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: SendRequestArgs = call.args()?;
    let mut options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        timeout: args.timeout_ms.map(Duration::from_millis),
        // Agents never write snapshot files: a missing snapshot fails.
        snapshots: SnapshotMode::Ci,
        ..RunOptions::default()
    };
    let ws = call.ctx.workspace();
    let sent = match (&args.path, &args.url) {
        (Some(path), None) => {
            let saved = super::workspace::find(&call.ctx, path)?;
            refuse_signing(&saved.entry.request)?;
            send_one(call, &saved, options).await?
        }
        (None, Some(url)) => {
            let entry = ad_hoc_entry(&args, url)?;
            call.ctx.learn_request_literals(&entry.request);
            let target = RequestRef {
                root: &ws.root,
                http: &ws.settings.http,
                entry: &entry,
                ancestors: Vec::new(),
            };
            options.ad_hoc = true;
            send_one(call, &target, options).await?
        }
        _ => {
            return Err(ToolError::new(
                ErrorCode::InvalidInput,
                "give either `path` (a saved request) or `url` (an ad-hoc request)",
            ));
        }
    };
    let result = send_result(&call.ctx, &sent);
    let line = match (result.status, &result.error) {
        (Some(status), _) => format!("{} {status} ({})", result.request, result.outcome),
        (None, Some(error)) => format!("{} {}: {error}", result.request, result.outcome),
        (None, None) => format!("{} {}", result.request, result.outcome),
    };
    output(&result, line)
}

/// The refusal of a saved `evm-tx` request (evm-signing "Agents can never
/// sign"): this crate does not link the signing gate, and the refusal comes
/// before anything is resolved or sent.
const EVM_TX_REFUSED: &str = "evm-tx requests cannot be sent through the agent interface";

fn refuse_signing(request: &Request) -> Result<(), ToolError> {
    match request {
        Request::Evm(evm) if matches!(**evm, EvmRequest::Tx(_)) => {
            Err(ToolError::new(ErrorCode::Blocked, EVM_TX_REFUSED))
        }
        _ => Ok(()),
    }
}

pub(super) fn outcome(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Passed => "passed",
        RequestStatus::Failed => "failed",
        RequestStatus::Errored => "errored",
        RequestStatus::Skipped => "skipped",
    }
}

/// The tool result of `sent`: the record's redacted fields, and the body
/// redacted with the session's and the exchange's secrets.
fn send_result(ctx: &AgentContext, sent: &Sent) -> SendResult {
    ctx.learn(&sent.redactor);
    let record: &RequestRecord = &sent.record;
    let exchange = record.exchange.as_ref();
    let headers = exchange
        .map(|e| &e.response.headers)
        .into_iter()
        .flatten()
        .map(|h| HeaderView {
            name: h.name.clone(),
            value: h.value.clone(),
        })
        .collect();
    let body = sent.exchange.as_ref().map(|e| {
        let session = ctx.redactor();
        let redacted = session.redactor().bytes(&e.body).into_owned();
        let redacted = sent.redactor.bytes(&redacted).into_owned();
        ctx.bodies().view(redacted.into(), e.size_bytes, e.binary)
    });
    let timings = exchange.map(|e| Timings {
        total_ms: e.timings.total_ms,
        ttfb_ms: e.timings.ttfb_ms,
        download_ms: e.timings.download_ms,
        dns_ms: e.timings.dns_ms,
        connect_ms: e.timings.connect_ms,
        tls_ms: e.timings.tls_ms,
    });
    SendResult {
        request: record.id.clone(),
        outcome: outcome(record.status).to_owned(),
        method: exchange.map(|e| e.request.method.clone()),
        url: exchange.map(|e| e.request.url.clone()),
        status: exchange.map(|e| e.response.status),
        error: record.error.clone(),
        headers,
        body,
        timings,
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

/// `path` relative to the workspace root, refusing anything that could
/// leave it.
pub fn contained(path: &str) -> Result<PathBuf, ToolError> {
    let relative = Path::new(path);
    let escapes = relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
    if escapes {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            format!("'{path}' is not a path inside the workspace"),
        ));
    }
    Ok(relative
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect())
}

/// A gate that counts each request it passes on as job progress.
struct Counting<'a> {
    inner: &'a dyn SendGate,
    job: &'a Job,
}

impl SendGate for Counting<'_> {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            let sent = self.inner.send(request, options).await;
            self.job.advance();
            sent
        })
    }

    fn authorize_kind<'a>(
        &'a self,
        request: &'a ResolvedRequest,
    ) -> BoxFuture<'a, Result<(), RequestError>> {
        Box::pin(async move {
            let admitted = self.inner.authorize_kind(request).await;
            self.job.advance();
            admitted
        })
    }
}

/// The name the client gave at `initialize`.
fn client_name(peer: &rmcp::service::Peer<rmcp::service::RoleServer>) -> String {
    peer.peer_info()
        .map(|info| info.client_info.name.clone())
        .unwrap_or_default()
}

/// What a run executes, checked before the job starts.
struct RunScope {
    scopes: Vec<PathBuf>,
    filters: Vec<Filter>,
    overrides: Variables,
    bail: bool,
}

fn run_scope(ctx: &AgentContext, args: &RunCollectionArgs) -> Result<(RunScope, usize), ToolError> {
    let scopes = match args.path.as_deref() {
        None | Some("" | ".") => Vec::new(),
        Some(path) => {
            let scope = contained(path)?;
            if !ctx.workspace().root.join(&scope).exists() {
                return Err(ToolError::new(
                    ErrorCode::NotFound,
                    format!("no file or folder '{path}'"),
                ));
            }
            vec![scope]
        }
    };
    let filters: Vec<Filter> = args.filter.iter().map(|f| Filter::parse(f)).collect();
    let count = plan(ctx.workspace(), &scopes, &filters).len();
    if count == 0 {
        return Err(ToolError::new(
            ErrorCode::NotFound,
            "no requests to run (check the path and filter)",
        ));
    }
    let scope = RunScope {
        scopes,
        filters,
        overrides: overrides(args.vars.as_ref())?,
        bail: args.bail,
    };
    Ok((scope, count))
}

pub fn run_collection(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: RunCollectionArgs = call.args()?;
    let ctx = call.ctx.clone();
    let (scope, requests) = run_scope(&ctx, &args)?;
    let max = usize::try_from(ctx.agent_settings().max_jobs.get()).unwrap_or(usize::MAX);
    let job = ctx.jobs().start(JobKind::Run, None, max)?;
    job.set_progress(0, requests);
    let peer = call.peer.clone();
    let owned = (ctx.clone(), job.clone());
    ctx.jobs()
        .spawn(&job, move || run_job(owned.0, peer, owned.1, scope));
    call.watch(&job);
    let started = JobStarted {
        job_id: job.id.clone(),
        requests,
    };
    let line = format!("{} started: {requests} requests", job.id);
    output(&started, line)
}

async fn run_job(
    ctx: Arc<AgentContext>,
    peer: rmcp::service::Peer<rmcp::service::RoleServer>,
    job: Arc<Job>,
    scope: RunScope,
) {
    let caller = JobCaller {
        client: client_name(&peer),
        tool: "run_collection",
        job_id: job.id.clone(),
    };
    let target = Target::default();
    let messaging = AgentMessaging::for_job(&ctx, &peer, &target, caller.clone());
    let agent = AgentGate::for_job(&ctx, &peer, &target, caller);
    let gate = Counting {
        inner: &agent,
        job: &job,
    };
    let environment = ctx.environment();
    let ws = ctx.workspace();
    let planned = plan(ws, &scope.scopes, &scope.filters);
    let options = RunOptions {
        overrides: scope.overrides,
        bail: scope.bail,
        cancel: job.cancel_token(),
        history: history(&ctx),
        gate: Some(&gate),
        messaging: Some(&messaging),
        snapshots: SnapshotMode::Ci,
        plugins: ctx.plugins(),
        ..RunOptions::default()
    };
    let run_ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: environment.as_ref(),
        secrets: ctx.secrets(),
        in_workspace: true,
    };
    let runner = match Runner::new(run_ctx, options) {
        Ok(runner) => ctx.with_providers(runner),
        Err(err) => return job.finish(JobState::Failed, Some(err.to_string()), None),
    };
    let record = runner.run(&planned).await;
    if let Err(err) = runner.finish() {
        tracing::warn!("cookies: {err}");
    }
    let report = Report::Run(Box::new(RunReport::new(&record))).to_json();
    let report = serde_json::from_str(&report).ok();
    let state = if record.summary.interrupted {
        JobState::Cancelled
    } else {
        JobState::Completed
    };
    job.finish(state, None, report);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_leave_the_workspace_are_refused() {
        assert_eq!(
            contained("users/./get.rman.yaml").unwrap(),
            PathBuf::from("users/get.rman.yaml")
        );
        for bad in ["../x", "users/../../x", "/etc/passwd"] {
            let err = contained(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput, "{bad}");
        }
    }

    #[test]
    fn ad_hoc_requests_take_method_headers_and_body() {
        let args = SendRequestArgs {
            method: Some("post".into()),
            headers: Some(BTreeMap::from([("Accept".into(), "text/plain".into())])),
            body: Some("hi".into()),
            ..SendRequestArgs::default()
        };
        let entry = ad_hoc_entry(&args, "http://127.0.0.1:1/x").unwrap();
        let Request::Http(http) = entry.request else {
            panic!("an HTTP request");
        };
        assert_eq!(http.effective_method(), "POST");
        assert_eq!(http.name, AD_HOC);
        assert!(overrides(Some(&BTreeMap::from([("id".into(), "42".into())]))).is_ok());
    }

    #[test]
    fn an_ad_hoc_url_is_never_read_as_a_curl_option() {
        for url in ["-d@/etc/passwd", " --data-binary=@x", "-K", "--config"] {
            let err = ad_hoc_entry(&SendRequestArgs::default(), url).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput, "{url}");
            assert!(
                err.message.contains("is not a URL"),
                "{url}: {}",
                err.message
            );
        }
    }
}
