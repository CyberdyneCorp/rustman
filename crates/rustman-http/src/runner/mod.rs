//! Running requests and collections (cli-runner spec, design D10): the engine
//! behind `rustman send` and `rustman run`, reusable by the TUI and the MCP
//! server.
//!
//! A [`Runner`] resolves each request with the run's captures, overrides and
//! persisted state, sends it, evaluates its assertions and applies its
//! captures, producing a redacted [`RequestRecord`]. [`Runner::run`] executes
//! a [`Plan`]: up to `parallel` units (folders) at a time, each unit in order,
//! with results kept in collection order. Cancelling [`RunOptions::cancel`]
//! (Ctrl+C) stops in-flight requests and skips the rest as `interrupted`.
//!
//! Every sink of a run (reports, history, error messages) redacts with the
//! secrets the whole run has seen so far, not only the request's own: a
//! secret of one request can reach a later one through a capture, and a
//! captured value holding a secret becomes a secret itself.
//!
//! With [`RunOptions::gate`] set (the agent interface), every resolved
//! request is handed to the [`SendGate`] instead of the engine, which
//! decides whether it may be sent and sends it under a safety policy.

mod execute;
mod export;
mod graphql;
mod grpc;
mod history;
mod jsonrpc;
mod mcp;
mod messaging;
mod plugins;
mod realtime;
mod record;
mod seed;
mod snapshots;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::{BoxFuture, join_all};
use rustman_core::captures::{PersistedState, RunCaptures};
use rustman_core::history::HistoryStore;
use rustman_core::model::Variables;
use rustman_core::run::{
    DataRowRecord, DataRun, Plan, RequestRecord, RunRecord, Summary, now_rfc3339,
};
use rustman_core::secrets::provider::{SecretProviders, resolve_with_secrets};
use rustman_core::settings::Settings;
use rustman_core::snapshot::{SnapshotMode, SnapshotReview};
use rustman_core::trace::{TraceLink, TracePropagation, WorkspaceScope};
use rustman_core::vars::{ResolveError, ResolvedGrpc, ResolvedRequest, Resolver};
use rustman_core::{Environment, Redactor, SecretStore};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use self::export::SpanExport;
pub use self::export::{FLUSH_WAIT, OTLP_BUILT, export_host, flush_all_exports};

use crate::HttpEngine;
use crate::cookies::CookieJar;
use crate::engine::SendOptions;
use crate::error::RequestError;
use crate::exchange::Exchange;
use crate::timing::millis;

pub use graphql::{GraphqlCall, GraphqlExecutor, GraphqlFuture, GraphqlOutcome};
pub use grpc::{
    GRPC_CONFIG_ERROR, GRPC_REQUEST_ERROR, GrpcCall, GrpcExecutor, GrpcFailure, GrpcFuture,
    GrpcOutcome,
};
pub use jsonrpc::{JsonrpcCall, JsonrpcCallCheck, JsonrpcExecutor, JsonrpcFuture, JsonrpcOutcome};
pub use mcp::{McpCall, McpExecutor, McpFuture, McpOutcome};
pub use messaging::{MessagingCall, MessagingExecutor, MessagingFuture, MessagingOutcome};
pub use realtime::{RealtimeCall, RealtimeExecutor, RealtimeFuture, RealtimeOutcome};
pub use record::{exchange_record, timings_record};

/// Where requests run: the workspace (or, for an ad-hoc request outside
/// one, the current folder with default settings).
#[derive(Clone, Copy)]
pub struct RunContext<'a> {
    pub root: &'a Path,
    pub settings: &'a Settings,
    pub environment: Option<&'a Environment>,
    pub secrets: &'a SecretStore,
    /// Inside a workspace: cookies are saved per environment, captures are
    /// persisted and large bodies spill to `.rustman/bodies/`. Outside one,
    /// cookies live for the run only and bodies spill to the temp folder.
    pub in_workspace: bool,
}

/// How the OAuth2 authorization-code flow may reach the user.
#[derive(Clone, Copy)]
pub enum LoginMode<'a> {
    /// A terminal is attached: show the authorization URL with this function.
    Interactive(&'a (dyn Fn(&str) + Send + Sync)),
    /// No terminal (stdin is not a TTY, as in CI): fail with the
    /// `rustman auth login` hint instead.
    NonInteractive,
}

/// Flags that change how a run executes.
pub struct RunOptions<'a> {
    /// `--var k=v`, the highest-precedence variables.
    pub overrides: Variables,
    /// `--timeout`: replaces every request's total timeout.
    pub timeout: Option<Duration>,
    /// `--insecure`: no TLS certificate verification.
    pub insecure: bool,
    /// `--bail`: skip every request not yet started after one fails or errors.
    pub bail: bool,
    /// What `--bail` skips: the rest of the run, or (`--bail=iteration`)
    /// the rest of the failing iteration only.
    pub bail_scope: BailScope,
    /// `--parallel`: how many units run at once (at least 1).
    pub parallel: usize,
    /// `--iterations`: how many times the plan runs (at least 1). Ignored
    /// with `data`, which runs once per row.
    pub iterations: u32,
    /// `--data`: run the plan once per row, with the row's values as
    /// variables. In-run captures are cleared at the start of each row.
    pub data: Option<DataRun>,
    pub login: LoginMode<'a>,
    /// Cancelled on Ctrl+C.
    pub cancel: CancellationToken,
    /// Where executed requests are recorded; `None` with `--no-history`.
    pub history: Option<HistoryStore>,
    /// The requests are ad-hoc (`rustman send <METHOD> <URL>`): history
    /// records them as `adhoc` instead of by file.
    pub ad_hoc: bool,
    /// Sends `kind: graphql` requests; without one they are skipped as an
    /// unsupported kind.
    pub graphql: Option<&'a dyn GraphqlExecutor>,
    /// Sends `kind: jsonrpc` requests; without one they are skipped as an
    /// unsupported kind.
    pub jsonrpc: Option<&'a dyn JsonrpcExecutor>,
    /// Sends `kind: grpc` requests; without one they are skipped as an
    /// unsupported kind.
    pub grpc: Option<&'a dyn GrpcExecutor>,
    /// How `snapshot` assertions treat their files: record, update
    /// (`--update-snapshots`) or CI (`--ci`, `CI=true`).
    pub snapshots: SnapshotMode,
    /// Sends `kind: mcp` requests, one session per server for the run;
    /// without one they are skipped as an unsupported kind.
    pub mcp: Option<&'a dyn McpExecutor>,
    /// Runs `kind: ws` and `kind: sse` sessions; without one they are
    /// skipped as an unsupported kind.
    pub realtime: Option<&'a dyn RealtimeExecutor>,
    /// Runs `kind: kafka`, `kind: mqtt` and `kind: amqp` operations;
    /// without one they are skipped as an unsupported kind.
    pub messaging: Option<&'a dyn MessagingExecutor>,
    /// Sends every request instead of the engine; `None` for the CLI and
    /// the TUI.
    pub gate: Option<&'a dyn SendGate>,
    /// `--trace`: inject trace headers even when no `trace:` block
    /// enables them.
    pub trace: bool,
    /// Called by [`Runner::run`] as each request finishes, with its record
    /// (already redacted) and every secret of the run so far, so a live
    /// view (`rustman run --viz`) can follow the run. Called from the run's
    /// tasks: it must not block.
    pub observer: Option<&'a RunObserver<'a>>,
    /// The workspace's trusted plugins: plugin auth schemes, assertion
    /// functions and value generators. Without them those are errors.
    pub plugins: Option<std::sync::Arc<dyn rustman_core::plugins::Extensions>>,
}

/// See [`RunOptions::observer`].
pub type RunObserver<'a> = dyn Fn(&RequestRecord, &Redactor) + Send + Sync + 'a;

/// Admits and sends the requests of a run: the agent interface checks each
/// one against the agent permissions, asks the human when needed and sends
/// it through a `GovernedSender`. OAuth2 tokens are not acquired on this
/// path.
pub trait SendGate: Send + Sync {
    /// Send `request`, or refuse it with [`RequestError::Blocked`].
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>>;

    /// Admit a request of a plugin kind before its plugin executes it, or
    /// refuse it with [`RequestError::Blocked`]. `request` is what history
    /// records for it: the kind as the method, `<kind>:<request id>` as the
    /// URL and the resolved fields as a JSON body. The plugin sends its own
    /// network requests (under its `net:` grants), so the gate cannot send
    /// it: it only decides, and audits.
    fn authorize_kind<'a>(
        &'a self,
        request: &'a ResolvedRequest,
    ) -> BoxFuture<'a, Result<(), RequestError>>;
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        Self {
            overrides: Variables::default(),
            timeout: None,
            insecure: false,
            bail: false,
            bail_scope: BailScope::Run,
            parallel: 1,
            iterations: 1,
            data: None,
            login: LoginMode::NonInteractive,
            cancel: CancellationToken::new(),
            history: None,
            ad_hoc: false,
            graphql: None,
            jsonrpc: None,
            mcp: None,
            grpc: None,
            realtime: None,
            messaging: None,
            gate: None,
            snapshots: SnapshotMode::Record,
            trace: false,
            observer: None,
            plugins: None,
        }
    }
}

/// What `--bail` skips after a failed or errored request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BailScope {
    /// Every request not yet started, in every iteration.
    #[default]
    Run,
    /// The rest of the failing iteration; the next one runs.
    Iteration,
}

/// What sending one request produced: its record and, when a response
/// arrived, the exchange itself (for `rustman send` to print the raw body).
pub struct Sent {
    pub record: RequestRecord,
    pub exchange: Option<Exchange>,
    /// The request as resolved, once resolution succeeded (for history).
    resolved: Option<ResolvedRequest>,
    /// The spill file of the response when no history entry keeps it.
    discard: Option<SpillFile>,
    /// Every secret of the run, and the credentials of this exchange: what
    /// printing the response must redact.
    pub redactor: Redactor,
    /// The failed, missing and new snapshots of this exchange, for a review
    /// after the send (redacted).
    pub snapshots: Vec<SnapshotReview>,
    /// What an mcp request sent, until its history entry is written.
    mcp_sent: Option<rustman_core::run::SentRequest>,
    /// The resolved grpc request, whose history entry records the call
    /// (`GRPC <target>/<method>`) instead of an HTTP request.
    grpc: Option<Box<ResolvedGrpc>>,
}

impl Sent {
    /// The request as resolved, once resolution succeeded. It holds secret
    /// values: never print or record it.
    pub fn resolved(&self) -> Option<&ResolvedRequest> {
        self.resolved.as_ref()
    }

    /// Delete the response's spill file once this result is dropped (after
    /// `rustman send` printed it), unless a history entry keeps it.
    fn discard_spill_unless(&mut self, kept: bool) {
        if !kept {
            let spill = self.exchange.as_ref().and_then(|e| e.spill.clone());
            self.discard = spill.map(SpillFile);
        }
    }
}

/// A spilled body nothing refers to: it holds the raw response, so it is
/// deleted when dropped.
struct SpillFile(PathBuf);

impl Drop for SpillFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Executes requests with shared captures, cookies and connections.
pub struct Runner<'a> {
    engine: HttpEngine,
    ctx: RunContext<'a>,
    options: RunOptions<'a>,
    jar: CookieJar,
    state: PersistedState,
    captures: Mutex<RunCaptures>,
    /// Every secret value resolved or captured so far in this run.
    known: Mutex<Redactor>,
    /// What `known` held once seeded: the data rows' and the environment's
    /// secrets, which [`Runner::forget_iteration_secrets`] keeps.
    baseline: Mutex<Redactor>,
    /// Whether `known` holds the secrets of the run's requests yet.
    seeded: AtomicBool,
    bailed: AtomicBool,
    /// Trace propagation for the whole run; under `scope: run` it holds
    /// the run's trace id.
    trace: Option<TracePropagation>,
    /// The trace link template recorded exchanges are linked with.
    link: Option<TraceLink>,
    /// `[trace.export]`: the client span of each exchange.
    export: SpanExport,
    /// Where `{{secret:...}}` references are fetched from when a request
    /// is sent; without them every reference is unresolved.
    secret_providers: Option<Arc<SecretProviders>>,
    /// The subscription signals of `start: before_next` consumes, by
    /// request id, until the consume takes its own.
    ready: Mutex<Vec<(String, tokio::sync::oneshot::Sender<()>)>>,
}

impl<'a> Runner<'a> {
    /// A runner with the cookie jar and persisted captures of the active
    /// environment.
    pub fn new(ctx: RunContext<'a>, options: RunOptions<'a>) -> Result<Self, RequestError> {
        let env = ctx.environment.map(|e| e.name.as_str());
        let (jar, state) = if ctx.in_workspace {
            let state = PersistedState::load(ctx.root, env)
                .map_err(|e| RequestError::Io(format!("cannot read persisted captures: {e}")))?;
            (CookieJar::for_environment(ctx.root, env)?, state)
        } else {
            (CookieJar::in_memory(), PersistedState::default())
        };
        let scope = WorkspaceScope {
            settings: ctx.settings,
            environment: ctx.environment,
            secrets: ctx.secrets,
            overrides: &options.overrides,
        };
        let trace = scope
            .propagation(options.trace)
            .map_err(RequestError::Invalid)?;
        let (trace, trace_secrets) = trace.unzip();
        let (link, link_secrets) = trace_link(&scope).unzip();
        let export = SpanExport::new(&scope.config());
        let runner = Self {
            engine: HttpEngine::new(),
            ctx,
            options,
            jar,
            state,
            captures: Mutex::new(RunCaptures::new()),
            known: Mutex::new(Redactor::new()),
            baseline: Mutex::new(Redactor::new()),
            seeded: AtomicBool::new(false),
            bailed: AtomicBool::new(false),
            trace,
            link,
            export,
            secret_providers: None,
            ready: Mutex::new(Vec::new()),
        };
        runner.remember(&trace_secrets.unwrap_or_default());
        runner.remember(&link_secrets.unwrap_or_default());
        // A plugin may return any secret of its session (one it is granted
        // in a header, a generated value or a body), so every sink redacts
        // all of them, whether or not a request references them.
        if let Some(plugins) = &runner.options.plugins {
            runner.remember_all(&plugins.redactor());
        }
        // Every secret column value is redacted from the first request on.
        if let Some(data) = &runner.options.data {
            runner.remember(&data.rows.secrets(&data.secret));
        }
        runner.keep_as_baseline();
        Ok(runner)
    }

    /// Resolve `{{secret:...}}` references through `providers` (shared
    /// with the rest of the process, so their cache is too).
    #[must_use]
    pub fn with_secret_providers(mut self, providers: Arc<SecretProviders>) -> Self {
        self.secret_providers = Some(providers);
        self
    }

    /// Resolve with `resolve`, fetching the provider secrets it needs:
    /// the only place a send contacts a secret provider.
    pub(crate) async fn with_secrets<T>(
        &self,
        resolver: &mut Resolver<'_>,
        resolve: impl Fn(&Resolver<'_>) -> Result<T, ResolveError>,
    ) -> Result<T, ResolveError> {
        let providers = self.secret_providers.as_deref();
        resolve_with_secrets(resolver, providers, resolve).await
    }

    /// Execute every request of `plan`, `iterations` times.
    pub async fn run(&self, plan: &Plan<'_>) -> RunRecord {
        self.seed(&plan.requests);
        let started_at = now_rfc3339();
        let start = Instant::now();
        let mut requests = Vec::with_capacity(plan.len());
        for iteration in 1..=self.iterations() {
            self.start_iteration();
            requests.extend(self.iteration(plan, iteration).await);
        }
        let interrupted = self.options.cancel.is_cancelled();
        RunRecord {
            started_at,
            environment: self.ctx.environment.map(|e| e.name.clone()),
            summary: Summary::of(&requests, millis(start.elapsed()), interrupted),
            requests,
        }
    }

    /// How many times the plan runs: once per data row, else `--iterations`.
    fn iterations(&self) -> u32 {
        match &self.options.data {
            Some(data) => data.total(),
            None => self.options.iterations.max(1),
        }
    }

    /// Iterations of a data-driven run are independent: they start without
    /// the in-run captures of the previous row (persisted ones stay), and
    /// under `--bail=iteration` without its bail.
    fn start_iteration(&self) {
        if self.options.data.is_some() {
            *self.lock_captures() = RunCaptures::new();
        }
        if self.options.bail_scope == BailScope::Iteration {
            self.bailed.store(false, Ordering::SeqCst);
        }
    }

    /// The data row of `iteration` in reports; `None` without `--data`.
    fn data_row(&self, iteration: u32) -> Option<DataRowRecord> {
        self.options
            .data
            .as_ref()
            .map(|data| data.data_row(iteration))
    }

    /// Save the cookie jar. Call once the run is over.
    pub fn finish(&self) -> Result<(), RequestError> {
        self.jar.save()
    }

    /// Send the spans of this run's exchanges now, waiting at most `wait`
    /// (`[trace.export]`); a no-op without export.
    pub fn flush_spans(&self, wait: Duration) {
        self.export.flush(wait);
    }

    /// Warnings about span export since the last call: a collector that
    /// failed (at most one a minute), or a build without the `otlp` feature.
    pub fn export_warnings(&self) -> Vec<String> {
        self.export.take_warnings()
    }

    async fn iteration(&self, plan: &Plan<'_>, iteration: u32) -> Vec<RequestRecord> {
        let parallel = self.options.parallel.max(1);
        let slots = Mutex::new(vec![None; plan.len()]);
        let semaphore = Semaphore::new(parallel);
        let units = plan.units(parallel);
        let tasks = units
            .iter()
            .map(|unit| self.unit(plan, unit, iteration, &semaphore, &slots));
        join_all(tasks).await;
        let slots = slots.into_inner().unwrap_or_else(PoisonError::into_inner);
        slots.into_iter().flatten().collect()
    }

    /// Run the requests of one unit in order, once a slot is free.
    async fn unit(
        &self,
        plan: &Plan<'_>,
        unit: &[usize],
        iteration: u32,
        semaphore: &Semaphore,
        slots: &Mutex<Vec<Option<RequestRecord>>>,
    ) {
        // The semaphore is never closed, so acquiring cannot fail.
        let _permit = semaphore.acquire().await.ok();
        let mut rest = unit.iter().copied().peekable();
        while let Some(index) = rest.next() {
            let request = &plan.requests[index];
            let next = rest.peek().copied();
            match next.filter(|_| messaging::starts_before_next(request)) {
                Some(next) => {
                    rest.next();
                    let (consumed, after) = self.consume_around(plan, index, next, iteration).await;
                    self.keep(slots, index, consumed);
                    self.keep(slots, next, after);
                }
                None => {
                    let sent = self.send(request, iteration).await;
                    self.keep(slots, index, sent);
                }
            }
        }
    }

    /// Run the `start: before_next` consume `consume`: once it is
    /// subscribed (or done), run `next` while it collects, and finish when
    /// both have.
    async fn consume_around(
        &self,
        plan: &Plan<'_>,
        consume: usize,
        next: usize,
        iteration: u32,
    ) -> (Sent, Sent) {
        let (ready, subscribed) = tokio::sync::oneshot::channel();
        self.leave_ready(&plan.requests[consume], ready);
        let mut consumed = Box::pin(self.send(&plan.requests[consume], iteration));
        let early = tokio::select! {
            sent = &mut consumed => Some(sent),
            _ = subscribed => None,
        };
        let after = self.send(&plan.requests[next], iteration);
        match early {
            Some(sent) => (sent, after.await),
            None => futures_util::future::join(consumed, after).await,
        }
    }

    /// Report `sent` to the observer and keep its record in slot `index`.
    fn keep(&self, slots: &Mutex<Vec<Option<RequestRecord>>>, index: usize, sent: Sent) {
        if let Some(observer) = self.options.observer {
            observer(&sent.record, &sent.redactor);
        }
        let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots[index] = Some(sent.record);
    }
}

/// The configured trace link. A link that cannot be resolved (a missing
/// variable) only loses the links: sending is unaffected.
fn trace_link(
    scope: &WorkspaceScope<'_>,
) -> Option<(TraceLink, Vec<rustman_core::vars::UsedSecret>)> {
    scope.link().unwrap_or_else(|e| {
        tracing::warn!("{e}; exchanges are recorded without a trace link");
        None
    })
}
