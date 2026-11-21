//! Sending a tab's request (spec: Non-blocking send, Cancel in-flight
//! request; design D3).
//!
//! `update` turns `request.send` into an [`Effect::Send`] carrying a
//! [`SendJob`]: the edited request, a copy of the workspace and the tab's
//! cancellation token. The effect runner executes it on a blocking thread
//! with a runtime of its own, through the engine's runner
//! (`rustman_http::runner`), which resolves variables, applies the safety
//! settings, records history and redacts the record. Cancelling the token
//! drops the request future, which closes its connection; the runtime and
//! its connection pool go away with the job.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustman_core::captures::{self, RunCaptures};
use rustman_core::collection::{Collection, Entry, RequestEntry};
use rustman_core::history::HistoryStore;
use rustman_core::model::{HttpRequest, REQUEST_FILE_SUFFIX, Request};
use rustman_core::run::RequestRecord;
use rustman_core::secrets::{BACKEND_VAR, SecretBackend, SecretStore};
use rustman_core::settings::Settings;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::Lookup;
use rustman_core::workspace::RequestRef;
use rustman_core::{Redactor, Workspace};
use rustman_evm::gql::RpcHead;
use rustman_graphql::{GraphqlRunner, Interactive, Paginate, SubscriptionEvent};
use rustman_grpc::GrpcRunner;
use rustman_http::Exchange;
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
use rustman_jsonrpc::JsonrpcRunner;
use rustman_mcp::client::oauth::OAuthOptions;
use rustman_mcp::client::{ConnectOptions, McpRunner};
use rustman_messaging::MessagingRunner;
use rustman_realtime::RealtimeRunner;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::app::{Action, Effect, Overlay, State};
use crate::dialogs::Confirm;
use crate::messaging::ConsumerLink;
use crate::realtime::LiveLink;
use crate::response::graphql::GraphqlView;
use crate::response::grpc::GrpcView;
use crate::response::jsonrpc::RpcView;
use crate::response::timing::SPARK_POINTS;
use crate::response::{Captured, RawBody, Received, Source};
use crate::review::Reviews;
use crate::tabs::{LiveEvent, LiveEvents, TabId, Typed};
use crate::toast::Severity;
use crate::workbench;

/// The file name an unsaved tab is sent (and recorded) under.
const UNTITLED: &str = "untitled";

/// Everything a send needs, owned, so it can run on another thread.
#[derive(Clone)]
pub struct SendJob {
    pub tab: TabId,
    pub seq: u64,
    pub request: Request,
    /// The tab's file, relative to the workspace root.
    pub path: Option<PathBuf>,
    /// `None` in a scratch session.
    pub workspace: Option<Box<Workspace>>,
    pub environment: Option<String>,
    pub cancel: CancellationToken,
    /// The secret store backend; `None`: as `RUSTMAN_SECRET_STORE` says.
    pub backend: Option<SecretBackend>,
    /// Check a GraphQL document before sending (off after "Send anyway").
    pub validate: bool,
    /// Stops a GraphQL subscription, keeping what arrived.
    pub stop: CancellationToken,
    /// Where a subscription's events go as they arrive.
    pub live: LiveEvents,
    /// How snapshot assertions treat their files.
    pub snapshots: SnapshotMode,
    /// Messages typed while a gRPC bidi call runs.
    pub typed: Typed,
    /// A realtime tab's session: where its entries go and its frames.
    pub realtime: Option<LiveLink>,
    /// The plugin trust store: a send in a workspace whose plugin set is
    /// trusted opens it (auth schemes, generators, transformers,
    /// assertions and kinds); an untrusted set stays unloaded.
    pub plugins: crate::plugin_trust::Gate,
    /// A messaging tab's consumer: where its messages go.
    pub messaging: Option<ConsumerLink>,
    /// `graphql.paginate`: follow subgraph pagination (`id_gt`).
    pub paginate: bool,
}

impl fmt::Debug for SendJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendJob")
            .field("tab", &self.tab)
            .field("seq", &self.seq)
            .field("path", &self.path)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SendJob {
    fn eq(&self, other: &Self) -> bool {
        (self.tab, self.seq) == (other.tab, other.seq)
    }
}

impl Eq for SendJob {}

/// `request.send`: start the current tab's request in the background. A
/// GraphQL document with errors asks first (spec: Pre-send validation).
pub fn start(state: &mut State) -> Vec<Effect> {
    start_with(state, true)
}

/// Send the current tab's request; `validate: false` skips the GraphQL
/// document check (the user chose "Send anyway").
pub fn start_with(state: &mut State, validate: bool) -> Vec<Effect> {
    if crate::realtime::is_connected(state) {
        crate::realtime::send_frame(state);
        return Vec::new();
    }
    let workspace = workbench::workspace(state).cloned().map(Box::new);
    let environment = state.status.environment.clone();
    let now = state.now;
    let Some(tab) = state.tabs.active_mut() else {
        state.notify(Severity::Info, "Open a request to send it");
        return Vec::new();
    };
    let request = match tab.editor.request() {
        Ok(request) => request,
        Err(message) => {
            state.notify(Severity::Error, format!("Cannot send: {message}"));
            return Vec::new();
        }
    };
    if validate && let Some(error) = graphql_error(tab) {
        let confirm = Confirm::invalid_graphql(tab.id, &error);
        state.overlay = Some(Overlay::Confirm(confirm));
        return Vec::new();
    }
    let (seq, cancel) = tab.start(now);
    let realtime = tab.editor.realtime_mut().map(|e| e.live.connect());
    let messaging = tab.editor.messaging_mut().map(|e| {
        e.selected = None;
        let (log, running) = e.consumer.connect();
        ConsumerLink { log, running }
    });
    let flight = tab.in_flight.as_ref().expect("just started");
    let job = SendJob {
        tab: tab.id,
        seq,
        request,
        path: tab.path.clone(),
        workspace,
        environment,
        cancel,
        backend: state.secret_backend,
        validate,
        stop: flight.stop.clone(),
        live: Arc::clone(&flight.live),
        snapshots: state.snapshot_mode,
        typed: Arc::clone(&flight.typed),
        realtime,
        plugins: state.plugin_gate.clone(),
        messaging,
        paginate: false,
    };
    state.status.in_flight = state.tabs.in_flight();
    vec![Effect::Send(Box::new(job))]
}

/// `graphql.paginate`: send the current GraphQL tab following subgraph
/// pagination (`id_gt`, at most 20 pages), the pages concatenated.
pub fn start_paged(state: &mut State) -> Vec<Effect> {
    if state
        .tabs
        .active()
        .and_then(|t| t.editor.graphql())
        .is_none()
    {
        state.notify(Severity::Info, "Pagination applies to GraphQL requests");
        return Vec::new();
    }
    let mut effects = start(state);
    for effect in &mut effects {
        if let Effect::Send(job) = effect {
            job.paginate = true;
        }
    }
    effects
}

/// The first error of a GraphQL tab's document, which blocks its send.
fn graphql_error(tab: &crate::tabs::Tab) -> Option<String> {
    let editor = tab.editor.graphql()?;
    let error = editor.validation.errors().next()?;
    Some(error.to_string())
}

/// `Esc` / `Ctrl+C`: cancel the current tab's request; false when it has
/// none in flight.
pub fn cancel(state: &mut State) -> bool {
    let now = state.now;
    let cancelled = state.tabs.active_mut().is_some_and(|tab| tab.cancel(now));
    state.status.in_flight = state.tabs.in_flight();
    cancelled
}

/// A send finished: show it in its tab unless it was cancelled meanwhile.
pub fn finished(state: &mut State, tab: TabId, seq: u64, mut received: Received) {
    for warning in std::mem::take(&mut received.export_warnings) {
        state.notify(Severity::Warn, warning);
    }
    let warnings = received.record.warnings.clone();
    if let Some(evm) = received.record.evm.clone() {
        if let Some(status) = evm["status"].as_str() {
            state.status.chain = Some(status.to_owned());
        }
        if let Some(editor) = state.tabs.get_mut(tab).and_then(|t| t.editor.evm_mut()) {
            editor.set_result(&evm);
        }
    }
    let shown = state
        .tabs
        .get_mut(tab)
        .is_some_and(|t| t.finish(seq, received));
    state.status.in_flight = state.tabs.in_flight();
    if shown {
        for warning in warnings {
            state.notify(Severity::Warn, warning);
        }
    }
}

/// Run `job` to completion. Blocks: the effect runner calls it on a
/// blocking thread.
pub fn execute(job: &SendJob) -> Action {
    let received = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(send(job)),
        Err(e) => failed(job, format!("cannot start the async runtime: {e}")),
    };
    Action::SendFinished {
        tab: job.tab,
        seq: job.seq,
        received: Box::new(received),
    }
}

/// A send that could not start.
fn failed(job: &SendJob, message: String) -> Received {
    if let Some(link) = &job.messaging {
        link.running.cancel();
    }
    let (id, file) = names(job.path.as_deref());
    let record = RequestRecord::new(id, job.request.name().to_owned(), file);
    Received::without_body(record.errored(message))
}

/// The id and file a request is recorded under.
pub(crate) fn names(path: Option<&Path>) -> (String, String) {
    let file = path.map_or_else(
        || format!("{UNTITLED}{REQUEST_FILE_SUFFIX}"),
        |p| p.to_string_lossy().replace('\\', "/"),
    );
    let id = file
        .strip_suffix(REQUEST_FILE_SUFFIX)
        .unwrap_or(&file)
        .to_owned();
    (id, file)
}

/// The workspace's secret store with `backend` (`None`: as
/// `RUSTMAN_SECRET_STORE` says).
pub(crate) fn secret_store(
    root: &Path,
    backend: Option<SecretBackend>,
) -> Result<SecretStore, String> {
    let backend = match backend {
        Some(backend) => backend,
        None => SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
            .map_err(|e| e.to_string())?,
    };
    Ok(SecretStore::open(root, backend))
}

async fn send(job: &SendJob) -> Received {
    match &job.workspace {
        Some(ws) => send_in_workspace(job, ws).await,
        None => send_scratch(job).await,
    }
}

/// Replace the request at `path` in the loaded tree with the edited one.
pub(crate) fn replace(collection: &mut Collection, path: &Path, request: &Request) -> bool {
    collection.entries.iter_mut().any(|entry| match entry {
        Entry::Request(r) if r.path == path => {
            r.request = request.clone();
            true
        }
        Entry::Request(_) => false,
        Entry::Collection(child) => replace(child, path, request),
    })
}

/// A copy of `ws` with the edited `request` in place of the file at
/// `path`; the path, when the file is part of the loaded tree.
pub(crate) fn with_edit(
    ws: &Workspace,
    path: Option<&Path>,
    request: &Request,
) -> (Workspace, Option<PathBuf>) {
    let mut ws = ws.clone();
    let path = path
        .map(Path::to_path_buf)
        .filter(|p| replace(&mut ws.tree, p, request));
    (ws, path)
}

/// The request at `path` in `ws`, or `loose` (a request outside the tree)
/// with the workspace root's settings.
pub(crate) fn target<'a>(
    ws: &'a Workspace,
    path: Option<&Path>,
    loose: &'a RequestEntry,
) -> RequestRef<'a> {
    let found = path.and_then(|p| ws.requests().into_iter().find(|r| r.entry.path == p));
    found.unwrap_or_else(|| RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry: loose,
        ancestors: vec![&ws.tree],
    })
}

/// The entry an unsaved (or moved) request is sent as.
pub(crate) fn loose_entry(path: Option<&Path>, request: &Request) -> RequestEntry {
    let (_, file) = names(path);
    RequestEntry {
        path: PathBuf::from(&file),
        request: request.clone(),
    }
}

async fn send_in_workspace(job: &SendJob, ws: &Workspace) -> Received {
    let (ws, path) = with_edit(ws, job.path.as_deref(), &job.request);
    let loose = loose_entry(job.path.as_deref(), &job.request);
    let target = target(&ws, path.as_deref(), &loose);
    let env = ws
        .environments
        .iter()
        .find(|e| job.environment.as_deref() == Some(e.name.as_str()));
    let place = Place {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        in_workspace: true,
    };
    run(job, &place, &target, path.is_none()).await
}

async fn send_scratch(job: &SendJob) -> Received {
    let root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => return failed(job, format!("no current directory: {e}")),
    };
    let settings = Settings::ad_hoc();
    let (_, file) = names(job.path.as_deref());
    let entry = RequestEntry {
        path: PathBuf::from(file),
        request: job.request.clone(),
    };
    let target = RequestRef {
        root: &root,
        http: &settings.http,
        entry: &entry,
        ancestors: Vec::new(),
    };
    let place = Place {
        root: &root,
        settings: &settings,
        environment: None,
        in_workspace: false,
    };
    run(job, &place, &target, true).await
}

/// The workspace's plugins, when its set is trusted on this machine: the
/// same extension points a headless command loads, with the session's
/// secrets and network. A scratch session has none.
fn open_plugins(
    job: &SendJob,
    place: &Place<'_>,
    store: &SecretStore,
) -> Result<Option<Arc<dyn rustman_core::plugins::Extensions>>, String> {
    match &job.plugins.0 {
        Some(gate) if place.in_workspace => gate.extensions(place.root, place.environment, store),
        _ => Ok(None),
    }
}

/// Where a request runs.
struct Place<'a> {
    root: &'a Path,
    settings: &'a Settings,
    environment: Option<&'a rustman_core::Environment>,
    in_workspace: bool,
}

impl Place<'_> {
    /// The workspace's history, when it records one (never in scratch).
    fn history(&self) -> Option<HistoryStore> {
        let settings = &self.settings.history;
        (self.in_workspace && settings.enabled)
            .then(|| HistoryStore::for_workspace(self.root, settings))
    }
}

async fn run(job: &SendJob, place: &Place<'_>, target: &RequestRef<'_>, ad_hoc: bool) -> Received {
    let store = match secret_store(place.root, job.backend) {
        Ok(store) => store,
        Err(message) => return failed(job, message),
    };
    let plugins = match open_plugins(job, place, &store) {
        Ok(plugins) => plugins,
        Err(message) => return failed(job, message),
    };
    let (feed, events) = unbounded_channel();
    let graphql = graphql_runner(job, feed);
    let (rpc_feed, rpc_events) = unbounded_channel();
    let jsonrpc = JsonrpcRunner {
        interactive: Some(rustman_jsonrpc::Interactive {
            stop: job.stop.clone(),
            feed: Some(rpc_feed),
        }),
        ..JsonrpcRunner::default()
    };
    let mcp = mcp_runner(place.root, job.backend);
    let (grpc_feed, grpc_entries) = unbounded_channel();
    let typed = typed_messages(job);
    let grpc = GrpcRunner {
        interactive: Some(Arc::new(rustman_grpc::Interactive::new(
            typed,
            Some(grpc_feed),
        ))),
        unbounded: true,
        ..GrpcRunner::default()
    };
    let (rt_feed, rt_entries) = unbounded_channel();
    let realtime = RealtimeRunner {
        interactive: Some(job.stop.clone()),
        feed: Some(rt_feed),
        commands: Mutex::new(job.realtime.as_ref().and_then(LiveLink::take_commands)),
        ..RealtimeRunner::default()
    };
    let (msg_feed, msg_entries) = unbounded_channel();
    let messaging = MessagingRunner {
        interactive: Some(job.stop.clone()),
        feed: Some(msg_feed),
        ..MessagingRunner::default()
    };
    let evm = rustman_evm::exec::EvmRunner::new(Arc::new(rustman_http::HttpEngine::new()));
    let options = RunOptions {
        cancel: job.cancel.clone(),
        history: place.history(),
        ad_hoc,
        evm: Some(&evm),
        graphql: Some(&graphql),
        jsonrpc: Some(&jsonrpc),
        mcp: Some(&mcp),
        grpc: Some(&grpc),
        realtime: Some(&realtime),
        messaging: Some(&messaging),
        snapshots: job.snapshots,
        plugins,
        ..RunOptions::default()
    };
    let ctx = RunContext {
        root: place.root,
        settings: place.settings,
        environment: place.environment,
        secrets: &store,
        in_workspace: place.in_workspace,
    };
    let runner = match build_runner(ctx, options, place) {
        Ok(runner) => runner,
        Err(message) => return failed(job, message),
    };
    let mut sent = tokio::select! {
        sent = runner.send(target, 1) => sent,
        () = collect(events, &job.live) => unreachable!("the feed outlives the send"),
        () = crate::jsonrpc::collect(rpc_events, &job.live) => {
            unreachable!("the feed outlives the send")
        }
        () = crate::grpc::collect(grpc_entries, &job.live) => {
            unreachable!("the feed outlives the send")
        }
        () = crate::realtime::collect(rt_entries, job.realtime.as_ref().map(|l| &l.log)) => {
            unreachable!("the feed outlives the send")
        }
        () = crate::messaging::collect(msg_entries, job.messaging.as_ref(), &job.stop) => {
            unreachable!("the feed outlives the send")
        }
    };
    stop_messaging(job);
    jsonrpc.close().await;
    mcp.shutdown().await;
    if let Err(e) = runner.finish() {
        sent.record.warnings.push(e.to_string());
    }
    let export_warnings = runner.export_warnings();
    let warnings = store.take_warnings();
    sent.record
        .warnings
        .extend(warnings.iter().map(|w| sent.redactor.text(w).into_owned()));
    let http = match target.request() {
        Request::Http(http) => Some(http.as_ref()),
        _ => None,
    };
    let reviews = std::mem::take(&mut sent.snapshots);
    let redactor = sent.redactor.clone();
    let mut received = prepare(sent, http);
    received.export_warnings = export_warnings;
    received.snapshots = Reviews::new(reviews, job.snapshots);
    received.graphql = graphql_view(&received, &redactor);
    received.jsonrpc = received
        .record
        .jsonrpc
        .as_ref()
        .map(|record| Box::new(RpcView::new(record)));
    received.grpc = received
        .record
        .grpc
        .as_ref()
        .map(|record| Box::new(GrpcView::new(record)));
    if !ad_hoc && let Some(store) = place.history() {
        received.history = latencies(&store, &received.record.file);
    }
    received
}

/// The runner of one send, with the workspace's secret providers.
fn build_runner<'a>(
    ctx: RunContext<'a>,
    options: RunOptions<'a>,
    place: &Place<'_>,
) -> Result<Runner<'a>, String> {
    let providers = crate::secret_providers::for_workspace(place.root, place.settings)?;
    let runner = Runner::new(ctx, options).map_err(|e| e.to_string())?;
    Ok(runner.with_secret_providers(providers))
}

/// The GraphQL runner of a send: subscriptions run until stopped, events
/// go to `feed`, subgraph freshness is read, and the job's pagination.
fn graphql_runner(job: &SendJob, feed: UnboundedSender<SubscriptionEvent>) -> GraphqlRunner {
    GraphqlRunner {
        validate: job.validate,
        interactive: Some(Interactive {
            stop: job.stop.clone(),
            feed: Some(feed),
        }),
        paginate: job.paginate.then(Paginate::default),
        chain_head: Some(Arc::new(RpcHead::new(Arc::new(
            rustman_http::HttpEngine::new(),
        )))),
        freshness: true,
        ..GraphqlRunner::default()
    }
}

/// Mark the job's live consumer as no longer running.
fn stop_messaging(job: &SendJob) {
    if let Some(link) = &job.messaging {
        link.running.cancel();
    }
}

/// Sends `kind: mcp` requests as `rustman run` does without `--trust`: a
/// stdio server must have been approved (in the MCP inspector), and OAuth
/// uses the tokens already stored (a needed login fails).
fn mcp_runner(root: &Path, backend: Option<SecretBackend>) -> McpRunner {
    let oauth = secret_store(root, backend).ok().map(|store| OAuthOptions {
        store: Arc::new(store),
        prompt: None,
    });
    let connect = ConnectOptions {
        oauth,
        ..ConnectOptions::default()
    };
    McpRunner::new(false, connect)
}

/// The messages typed during the job's gRPC bidi call, if not taken yet.
fn typed_messages(job: &SendJob) -> Option<UnboundedReceiver<serde_json::Value>> {
    job.typed.lock().ok().and_then(|mut typed| typed.take())
}

/// The GraphQL result of a GraphQL exchange, from its redacted record and
/// body (a body spooled to disk is only in the Body tab); its formatted
/// forms are redacted again with `redactor`.
fn graphql_view(received: &Received, redactor: &Redactor) -> Option<Box<GraphqlView>> {
    let record = received.record.graphql.as_ref()?;
    let body = match &received.source {
        Some(Source::Memory(bytes)) => Some(bytes.as_ref()),
        _ => None,
    };
    Some(Box::new(GraphqlView::new(record, body).redacted(redactor)))
}

/// Keep a subscription's events for the live view. Never returns: the
/// runner holds the feed's sender until the send is over.
async fn collect(mut events: UnboundedReceiver<SubscriptionEvent>, live: &LiveEvents) {
    while let Some(event) = events.recv().await {
        if let Ok(mut log) = live.lock() {
            log.push(LiveEvent {
                elapsed_ms: rustman_http::millis(event.elapsed),
                payload: event.payload,
            });
        }
    }
    std::future::pending::<()>().await;
}

/// The viewer's copy of what was sent: the redacted body (spooled to a
/// temp file when the engine spilled it), parsed, with captured values.
fn prepare(sent: Sent, http: Option<&HttpRequest>) -> Received {
    let Sent {
        record,
        exchange,
        redactor,
        ..
    } = sent;
    let Some(exchange) = exchange else {
        return Received::without_body(record);
    };
    let captured = captured(&record, &exchange, http, &redactor);
    let raw = match redacted_body(&exchange, &redactor) {
        Ok(source) => RawBody {
            source,
            size: exchange.size_bytes,
            binary: exchange.binary,
            content_type: content_type(&exchange),
        },
        Err(e) => {
            let mut received = Received::without_body(record);
            received
                .record
                .warnings
                .push(format!("cannot keep the response body: {e}"));
            return received;
        }
    };
    let mut received = Received::with_body(record, raw);
    received.captured = captured;
    received
}

fn content_type(exchange: &Exchange) -> Option<String> {
    exchange
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// The body with every secret replaced: in memory, or copied from the
/// engine's spill file into a spool file of the viewer's own.
fn redacted_body(exchange: &Exchange, redactor: &Redactor) -> std::io::Result<Source> {
    let Some(spill) = &exchange.spill else {
        let bytes = redactor.bytes(&exchange.body);
        return Ok(Source::Memory(Arc::from(bytes.as_ref())));
    };
    let mut spool = tempfile::Builder::new()
        .prefix("rustman-body-")
        .tempfile()?;
    redactor.copy(File::open(spill)?, spool.as_file_mut())?;
    Ok(Source::Spool(Arc::new(spool)))
}

/// The values the request's captures took, with the engine's own
/// capture logic; secret ones stay masked and the rest are redacted.
fn captured(
    record: &RequestRecord,
    exchange: &Exchange,
    http: Option<&HttpRequest>,
    redactor: &Redactor,
) -> Vec<Captured> {
    let rules = http.and_then(|h| h.captures.as_deref()).unwrap_or_default();
    if rules.is_empty() {
        return Vec::new();
    }
    let mut scope = RunCaptures::new();
    if let Ok(view) = exchange.view(true) {
        captures::apply(rules, &view, &mut scope, None);
    }
    record
        .captures
        .iter()
        .map(|result| {
            let value = match scope.get(&result.var) {
                Some(Lookup::Value(v)) if !result.secret && !redactor.reveals_secret(v) => {
                    Some(redactor.text(v).into_owned())
                }
                _ => None,
            };
            Captured {
                secret: result.secret || (value.is_none() && result.message.is_none()),
                var: result.var.clone(),
                value,
                message: result.message.clone(),
            }
        })
        .collect()
}

/// Total times of the latest exchanges of `file` in history, oldest first.
fn latencies(store: &HistoryStore, file: &str) -> Vec<f64> {
    let Ok(listing) = store.list() else {
        return Vec::new();
    };
    let mut points: Vec<f64> = listing
        .entries
        .iter()
        .filter(|e| e.request == file)
        .filter_map(|e| e.total_ms)
        .take(SPARK_POINTS)
        .collect();
    points.reverse();
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_file() {
        assert_eq!(
            names(Some(Path::new("users/get.rman.yaml"))),
            ("users/get".to_owned(), "users/get.rman.yaml".to_owned())
        );
        assert_eq!(names(None).1, "untitled.rman.yaml");
    }

    #[test]
    fn jobs_compare_by_tab_and_send() {
        let request = rustman_core::model::KindRegistry::default()
            .load(
                Path::new("r"),
                "version: 1\nkind: http\nname: R\nurl: http://x/\n",
            )
            .unwrap();
        let job = SendJob {
            tab: 1,
            seq: 1,
            request,
            path: None,
            workspace: None,
            environment: None,
            cancel: CancellationToken::new(),
            backend: None,
            validate: true,
            stop: CancellationToken::new(),
            live: LiveEvents::default(),
            snapshots: SnapshotMode::Record,
            typed: Typed::default(),
            realtime: None,
            plugins: crate::plugin_trust::Gate::default(),
            messaging: None,
            paginate: false,
        };
        let mut other = job.clone();
        assert_eq!(job, other);
        other.seq = 2;
        assert_ne!(job, other);
        assert!(format!("{job:?}").contains("seq: 1"));
    }
}
