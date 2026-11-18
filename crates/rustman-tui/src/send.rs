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
use std::sync::Arc;

use rustman_core::captures::{self, RunCaptures};
use rustman_core::collection::{Collection, Entry, RequestEntry};
use rustman_core::history::HistoryStore;
use rustman_core::model::{HttpRequest, REQUEST_FILE_SUFFIX, Request};
use rustman_core::run::RequestRecord;
use rustman_core::secrets::{BACKEND_VAR, SecretBackend, SecretStore};
use rustman_core::settings::Settings;
use rustman_core::vars::Lookup;
use rustman_core::workspace::RequestRef;
use rustman_core::{Redactor, Workspace};
use rustman_http::Exchange;
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
use tokio_util::sync::CancellationToken;

use crate::app::{Action, Effect, State};
use crate::response::timing::SPARK_POINTS;
use crate::response::{Captured, RawBody, Received, Source};
use crate::tabs::TabId;
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

/// `request.send`: start the current tab's request in the background.
pub fn start(state: &mut State) -> Vec<Effect> {
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
    let (seq, cancel) = tab.start(now);
    let job = SendJob {
        tab: tab.id,
        seq,
        request,
        path: tab.path.clone(),
        workspace,
        environment,
        cancel,
        backend: state.secret_backend,
    };
    state.status.in_flight = state.tabs.in_flight();
    vec![Effect::Send(Box::new(job))]
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
pub fn finished(state: &mut State, tab: TabId, seq: u64, received: Received) {
    let warnings = received.record.warnings.clone();
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
    let (id, file) = names(job.path.as_deref());
    let record = RequestRecord::new(id, job.request.name().to_owned(), file);
    Received::without_body(record.errored(message))
}

/// The id and file a request is recorded under.
fn names(path: Option<&Path>) -> (String, String) {
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
fn replace(collection: &mut Collection, path: &Path, request: &Request) -> bool {
    collection.entries.iter_mut().any(|entry| match entry {
        Entry::Request(r) if r.path == path => {
            r.request = request.clone();
            true
        }
        Entry::Request(_) => false,
        Entry::Collection(child) => replace(child, path, request),
    })
}

async fn send_in_workspace(job: &SendJob, ws: &Workspace) -> Received {
    let mut ws = ws.clone();
    let path = job
        .path
        .clone()
        .filter(|p| replace(&mut ws.tree, p, &job.request));
    let (_, file) = names(job.path.as_deref());
    let loose = RequestEntry {
        path: PathBuf::from(&file),
        request: job.request.clone(),
    };
    let requests = ws.requests();
    let target = match &path {
        Some(p) => requests.into_iter().find(|r| r.entry.path == *p),
        None => None,
    };
    let target = target.unwrap_or_else(|| RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry: &loose,
        ancestors: vec![&ws.tree],
    });
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
    let options = RunOptions {
        cancel: job.cancel.clone(),
        history: place.history(),
        ad_hoc,
        ..RunOptions::default()
    };
    let ctx = RunContext {
        root: place.root,
        settings: place.settings,
        environment: place.environment,
        secrets: &store,
        in_workspace: place.in_workspace,
    };
    let runner = match Runner::new(ctx, options) {
        Ok(runner) => runner,
        Err(e) => return failed(job, e.to_string()),
    };
    let mut sent = runner.send(target, 1).await;
    if let Err(e) = runner.finish() {
        sent.record.warnings.push(e.to_string());
    }
    let warnings = store.take_warnings();
    sent.record
        .warnings
        .extend(warnings.iter().map(|w| sent.redactor.text(w).into_owned()));
    let http = match target.request() {
        Request::Http(http) => Some(http.as_ref()),
        _ => None,
    };
    let mut received = prepare(sent, http);
    if !ad_hoc && let Some(store) = place.history() {
        received.history = latencies(&store, &received.record.file);
    }
    received
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
        };
        let mut other = job.clone();
        assert_eq!(job, other);
        other.seq = 2;
        assert_ne!(job, other);
        assert!(format!("{job:?}").contains("seq: 1"));
    }
}
