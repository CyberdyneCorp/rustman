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
mod graphql;
mod grpc;
mod history;
mod jsonrpc;
mod mcp;
mod realtime;
mod record;
mod seed;
mod snapshots;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::{BoxFuture, join_all};
use rustman_core::captures::{PersistedState, RunCaptures};
use rustman_core::history::HistoryStore;
use rustman_core::model::Variables;
use rustman_core::run::{
    DataRowRecord, DataRun, Plan, RequestRecord, RunRecord, Summary, now_rfc3339,
};
use rustman_core::settings::Settings;
use rustman_core::snapshot::{SnapshotMode, SnapshotReview};
use rustman_core::vars::{ResolvedGrpc, ResolvedRequest};
use rustman_core::{Environment, Redactor, SecretStore};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

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
    /// Sends every request instead of the engine; `None` for the CLI and
    /// the TUI.
    pub gate: Option<&'a dyn SendGate>,
    /// Called by [`Runner::run`] as each request finishes, with its record
    /// (already redacted) and every secret of the run so far, so a live
    /// view (`rustman run --viz`) can follow the run. Called from the run's
    /// tasks: it must not block.
    pub observer: Option<&'a RunObserver<'a>>,
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
            gate: None,
            snapshots: SnapshotMode::Record,
            observer: None,
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
    /// Whether `known` holds the secrets of the run's requests yet.
    seeded: AtomicBool,
    bailed: AtomicBool,
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
        let runner = Self {
            engine: HttpEngine::new(),
            ctx,
            options,
            jar,
            state,
            captures: Mutex::new(RunCaptures::new()),
            known: Mutex::new(Redactor::new()),
            seeded: AtomicBool::new(false),
            bailed: AtomicBool::new(false),
        };
        // Every secret column value is redacted from the first request on.
        if let Some(data) = &runner.options.data {
            runner.remember(&data.rows.secrets(&data.secret));
        }
        Ok(runner)
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
        for &index in unit {
            let sent = self.send(&plan.requests[index], iteration).await;
            if let Some(observer) = self.options.observer {
                observer(&sent.record, &sent.redactor);
            }
            let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots[index] = Some(sent.record);
        }
    }
}
