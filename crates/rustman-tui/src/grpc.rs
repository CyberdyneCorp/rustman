//! The gRPC actions of the workbench (add-grpc-client "TUI gRPC pane"):
//! loading and refreshing a tab's schema off the render loop, sending
//! messages typed while a bidi call runs, half-closing it, and keeping the
//! live timeline of a running call. The editing itself is in
//! [`crate::grpc_pane`].
//!
//! A schema load resolves the request in the active environment, like a
//! send, so errors are redacted before they reach the screen. It reads
//! the request's `.proto` files or descriptor set, else the descriptor
//! cache, else asks the server by reflection (and caches the answer);
//! `Ctrl+G` asks the server again. An unreachable server with a cached
//! schema shows the cached one with a stale notice.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use rustman_core::Workspace;
use rustman_core::collection::RequestEntry;
use rustman_core::model::Request;
use rustman_core::secrets::SecretBackend;
use rustman_core::settings::Settings;
use rustman_core::vars::{Resolver, resolve_grpc};
use rustman_core::workspace::RequestRef;
use rustman_grpc::{DescriptorCache, TimelineEntry, schema_for};
use rustman_http::SendOptions;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Action, Effect, State};
use crate::grpc_pane::{LoadedSchema, SchemaState};
use crate::send::{loose_entry, names, secret_store, target, with_edit};
use crate::tabs::{LiveEvent, LiveEvents, TabId};
use crate::toast::Severity;
use crate::workbench;

/// What a schema load needs, owned, to run on another thread.
#[derive(Clone)]
pub struct SchemaJob {
    pub tab: TabId,
    /// Ask the server again instead of reading the cache.
    pub refresh: bool,
    pub request: Request,
    /// The tab's file, relative to the workspace root.
    pub path: Option<PathBuf>,
    /// `None` in a scratch session: nothing is cached there.
    pub workspace: Option<Box<Workspace>>,
    pub environment: Option<String>,
    pub backend: Option<SecretBackend>,
}

impl fmt::Debug for SchemaJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcSchemaJob")
            .field("tab", &self.tab)
            .field("refresh", &self.refresh)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SchemaJob {
    fn eq(&self, other: &Self) -> bool {
        (self.tab, self.refresh, &self.path) == (other.tab, other.refresh, &other.path)
    }
}

impl Eq for SchemaJob {}

/// Run `job`. Blocks: the effect runner calls it on a blocking thread.
pub fn execute(job: &SchemaJob) -> Action {
    let result = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(load(job)),
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    Action::GrpcSchema {
        tab: job.tab,
        refresh: job.refresh,
        result: result.map(Box::new),
    }
}

async fn load(job: &SchemaJob) -> Result<LoadedSchema, String> {
    match &job.workspace {
        Some(ws) => {
            let (ws, path) = with_edit(ws, job.path.as_deref(), &job.request);
            let loose = loose_entry(job.path.as_deref(), &job.request);
            let request = target(&ws, path.as_deref(), &loose);
            let cache = DescriptorCache::for_workspace(&ws.root);
            fetch(job, &ws.settings, &request, Some(cache)).await
        }
        None => {
            let root = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
            let settings = Settings::ad_hoc();
            let entry = RequestEntry {
                path: PathBuf::from(names(job.path.as_deref()).1),
                request: job.request.clone(),
            };
            let request = RequestRef {
                root: &root,
                http: &settings.http,
                entry: &entry,
                ancestors: Vec::new(),
            };
            fetch(job, &settings, &request, None).await
        }
    }
}

/// The schema of `request`, resolved in the job's environment.
async fn fetch(
    job: &SchemaJob,
    settings: &Settings,
    request: &RequestRef<'_>,
    cache: Option<DescriptorCache>,
) -> Result<LoadedSchema, String> {
    let store = secret_store(request.root, job.backend)?;
    let env = job.workspace.as_ref().and_then(|ws| {
        ws.environments
            .iter()
            .find(|e| job.environment.as_deref() == Some(e.name.as_str()))
    });
    let resolver = Resolver::for_request(settings, request, env, &store);
    let resolved = resolve_grpc(request, &resolver).map_err(|e| e.to_string())?;
    let redactor = resolved.redactor();
    let options = SendOptions::for_request(settings, request.root, &resolved.request)
        .map_err(|e| redactor.text(&e.to_string()).into_owned())?;
    let (schema, _) = schema_for(&resolved, &options, cache, job.refresh)
        .await
        .map_err(|e| e.redacted(&redactor).to_string())?;
    Ok(LoadedSchema::new(schema))
}

/// Start loading the schema of every gRPC tab that has not asked yet.
pub fn pending_loads(state: &mut State) -> Vec<Effect> {
    let waiting: Vec<TabId> = state
        .tabs
        .list
        .iter()
        .filter(|t| {
            t.editor
                .grpc()
                .is_some_and(|g| matches!(g.schema, SchemaState::Unloaded))
        })
        .map(|t| t.id)
        .collect();
    waiting
        .into_iter()
        .filter_map(|id| load_effect(state, id, false))
        .collect()
}

/// `Ctrl+G` on a gRPC tab: ask the server for its schema again.
pub fn refresh(state: &mut State) -> Vec<Effect> {
    let Some(id) = state.tabs.active().map(|t| t.id) else {
        return Vec::new();
    };
    match load_effect(state, id, true) {
        Some(effect) => {
            state.notify(Severity::Info, "Refreshing the gRPC schema…");
            vec![effect]
        }
        None => Vec::new(),
    }
}

/// The load of tab `id`'s schema; `None` when it is not a gRPC tab or its
/// request does not parse.
fn load_effect(state: &mut State, id: TabId, refresh: bool) -> Option<Effect> {
    let workspace = workbench::workspace(state).cloned().map(Box::new);
    let environment = state.status.environment.clone();
    let backend = state.secret_backend;
    let tab = state.tabs.get_mut(id)?;
    let request = tab.editor.request();
    let editor = tab.editor.grpc_mut()?;
    let request = match request {
        Ok(request) => request,
        Err(e) => {
            editor.schema = SchemaState::Failed(e);
            return None;
        }
    };
    editor.schema = SchemaState::Loading;
    Some(Effect::GrpcSchema(Box::new(SchemaJob {
        tab: id,
        refresh,
        request,
        path: tab.path.clone(),
        workspace,
        environment,
        backend,
    })))
}

/// A schema load finished: show it in its tab.
pub fn on_loaded(
    state: &mut State,
    id: TabId,
    refresh: bool,
    result: Result<Box<LoadedSchema>, String>,
) {
    let Some(editor) = state.tabs.get_mut(id).and_then(|t| t.editor.grpc_mut()) else {
        return;
    };
    let (schema, notice) = match result {
        Ok(loaded) => {
            let methods = loaded.methods.len();
            let notice = (
                Severity::Success,
                format!("Schema refreshed: {methods} methods"),
            );
            (SchemaState::Ready(Arc::from(loaded)), notice)
        }
        Err(message) => {
            let notice = (Severity::Error, format!("Schema not refreshed: {message}"));
            (SchemaState::Failed(message), notice)
        }
    };
    editor.schema_loaded(schema);
    if refresh {
        state.notify(notice.0, notice.1);
    }
}

/// Whether the active tab is a gRPC one.
pub fn is_active(state: &State) -> bool {
    state
        .tabs
        .active()
        .is_some_and(|t| t.editor.grpc().is_some())
}

/// `Ctrl+R` while a bidi call of the active tab runs: send the Message tab
/// (each entry of a list) on its stream. False when no bidi call is
/// running, so the key sends the request as usual.
pub fn send_typed(state: &mut State) -> bool {
    let Some(tab) = state.tabs.active() else {
        return false;
    };
    let Some(editor) = tab.editor.grpc().filter(|e| e.is_bidi()) else {
        return false;
    };
    let Some(outbound) = tab.in_flight.as_ref().and_then(|f| f.outbound.clone()) else {
        return false;
    };
    let messages = match editor.message_value() {
        Ok(Value::Array(list)) => list,
        Ok(one) => vec![one],
        Err(e) => {
            state.notify(Severity::Error, format!("Not sent: {e}"));
            return true;
        }
    };
    if let Some(loaded) = editor.loaded()
        && let Some(error) = messages
            .iter()
            .find_map(|m| loaded.check(&editor.method, m).err())
    {
        state.notify(Severity::Error, format!("Not sent: {error}"));
        return true;
    }
    let count = messages.len();
    let sent = messages.into_iter().all(|m| outbound.send(m).is_ok());
    if sent {
        state.notify(
            Severity::Info,
            format!("Sent {count} message(s) on the stream"),
        );
    } else {
        state.notify(Severity::Warn, "The stream is closed");
    }
    true
}

/// `Alt+X` on a gRPC tab: half-close the running call's stream (the
/// server may still answer). False when there is nothing to half-close.
pub fn half_close(state: &mut State) -> bool {
    let flight = state.tabs.active_mut().and_then(|t| t.in_flight.as_mut());
    let Some(flight) = flight.filter(|f| f.outbound.is_some()) else {
        state.notify(Severity::Info, "No gRPC stream is open");
        return false;
    };
    flight.outbound = None;
    state.notify(Severity::Info, "Half-closed: no more messages will be sent");
    true
}

/// Keep a gRPC call's (redacted) timeline entries for the live view.
/// Never returns: the runner holds the feed's sender until the send is over.
pub async fn collect(mut entries: UnboundedReceiver<TimelineEntry>, live: &LiveEvents) {
    while let Some(entry) = entries.recv().await {
        if let Ok(mut log) = live.lock() {
            log.push(LiveEvent {
                elapsed_ms: rustman_http::millis(entry.at),
                payload: json!({
                    "direction": entry.direction.label(),
                    "size_bytes": entry.size_bytes,
                    "message": entry.message,
                }),
            });
        }
    }
    std::future::pending::<()>().await;
}
