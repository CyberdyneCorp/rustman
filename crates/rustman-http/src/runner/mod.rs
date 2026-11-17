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

mod execute;
mod history;
mod record;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use rustman_core::captures::{PersistedState, RunCaptures};
use rustman_core::history::HistoryStore;
use rustman_core::model::Variables;
use rustman_core::run::{Plan, RequestRecord, RunRecord, Summary, now_rfc3339};
use rustman_core::settings::Settings;
use rustman_core::vars::ResolvedRequest;
use rustman_core::{Environment, Redactor, SecretStore};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::HttpEngine;
use crate::cookies::CookieJar;
use crate::error::RequestError;
use crate::exchange::Exchange;
use crate::timing::millis;

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
    /// `--parallel`: how many units run at once (at least 1).
    pub parallel: usize,
    /// `--iterations`: how many times the plan runs (at least 1).
    pub iterations: u32,
    pub login: LoginMode<'a>,
    /// Cancelled on Ctrl+C.
    pub cancel: CancellationToken,
    /// Where executed requests are recorded; `None` with `--no-history`.
    pub history: Option<HistoryStore>,
    /// The requests are ad-hoc (`rustman send <METHOD> <URL>`): history
    /// records them as `adhoc` instead of by file.
    pub ad_hoc: bool,
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        Self {
            overrides: Variables::default(),
            timeout: None,
            insecure: false,
            bail: false,
            parallel: 1,
            iterations: 1,
            login: LoginMode::NonInteractive,
            cancel: CancellationToken::new(),
            history: None,
            ad_hoc: false,
        }
    }
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
}

impl Sent {
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
        Ok(Self {
            engine: HttpEngine::new(),
            ctx,
            options,
            jar,
            state,
            captures: Mutex::new(RunCaptures::new()),
            known: Mutex::new(Redactor::new()),
            bailed: AtomicBool::new(false),
        })
    }

    /// Execute every request of `plan`, `iterations` times.
    pub async fn run(&self, plan: &Plan<'_>) -> RunRecord {
        let started_at = now_rfc3339();
        let start = Instant::now();
        let mut requests = Vec::with_capacity(plan.len());
        for iteration in 1..=self.options.iterations.max(1) {
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
            let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots[index] = Some(sent.record);
        }
    }
}
