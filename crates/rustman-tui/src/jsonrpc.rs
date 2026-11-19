//! The JSON-RPC actions of the workbench (add-jsonrpc-client "TUI JSON-RPC
//! pane"): finding each tab's OpenRPC catalog, the method completion
//! popup, and stopping a live subscription with `Esc`. The editing itself
//! is in [`crate::jsonrpc_pane`].

use std::sync::Arc;
use std::time::Instant;

use rustman_core::Workspace;
use rustman_jsonrpc::catalog::catalog_for;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::State;
use crate::jsonrpc_pane::{CatalogState, JsonrpcEditor};
use crate::tabs::{LiveEvent, LiveEvents, Tab};
use crate::toast::Severity;
use crate::vars_view::VarScope;
use crate::workbench;

/// Look up the catalog of every JSON-RPC tab that has not looked yet: the
/// `[jsonrpc.catalogs]` file or the `rustman rpc discover` cache. Only
/// local files are read, and the endpoint is resolved without secrets.
pub fn pending_catalogs(state: &mut State) {
    let waiting = state.tabs.list.iter().any(|t| {
        t.editor
            .jsonrpc()
            .is_some_and(|e| matches!(e.catalog, CatalogState::Unloaded))
    });
    if !waiting {
        return;
    }
    let workspace = workbench::workspace(state).cloned();
    let environment = state.status.environment.clone();
    for tab in &mut state.tabs.list {
        let path = tab.path.clone();
        let Some(editor) = tab.editor.jsonrpc_mut() else {
            continue;
        };
        if !matches!(editor.catalog, CatalogState::Unloaded) {
            continue;
        }
        editor.catalog = match &workspace {
            Some(ws) => lookup(ws, environment.as_deref(), path.as_deref(), editor),
            None => CatalogState::Ready(None),
        };
    }
}

/// The catalog of `editor`'s endpoint in `ws`.
fn lookup(
    ws: &Workspace,
    environment: Option<&str>,
    path: Option<&std::path::Path>,
    editor: &JsonrpcEditor,
) -> CatalogState {
    let written = editor.endpoint.text();
    let scope = VarScope::new(ws, environment, path, editor.base().variables.as_ref());
    let resolved = scope.resolve(&written).unwrap_or_else(|| written.clone());
    match catalog_for(&ws.root, &ws.settings.jsonrpc, &written, &resolved) {
        Ok(found) => CatalogState::Ready(found.map(Arc::new)),
        Err(e) => CatalogState::Failed(e.to_string()),
    }
}

/// Whether the active tab is a JSON-RPC one.
pub fn is_active(state: &State) -> bool {
    state
        .tabs
        .active()
        .is_some_and(|t| t.editor.jsonrpc().is_some())
}

/// `editor.complete` on a JSON-RPC tab: the catalog methods matching the
/// method typed so far.
pub fn complete(state: &mut State) {
    let Some(editor) = state.tabs.active_mut().and_then(|t| t.editor.jsonrpc_mut()) else {
        return;
    };
    if editor.open_completion() {
        state.shell.focus = crate::focus::Focus::EditorBody;
        return;
    }
    let why = if editor.loaded_catalog().is_none() {
        "No completions: the endpoint has no OpenRPC catalog (rustman rpc discover)"
    } else {
        "No method starts with what is typed"
    };
    state.notify(Severity::Info, why);
}

/// `Esc` on a JSON-RPC tab whose subscription is running: unsubscribe and
/// stop it, keeping the events so far (the exchange is then shown). False
/// when there is no running subscription, or it is already stopping, so
/// the next `Esc` cancels the request outright.
pub fn stop_subscription(state: &mut State) -> bool {
    let Some(tab) = state.tabs.active() else {
        return false;
    };
    if !is_subscription(tab) {
        return false;
    }
    let Some(flight) = tab.in_flight.as_ref().filter(|f| !f.stop.is_cancelled()) else {
        return false;
    };
    flight.stop.cancel();
    state.notify(Severity::Info, "Unsubscribing…");
    true
}

fn is_subscription(tab: &Tab) -> bool {
    tab.editor
        .jsonrpc()
        .is_some_and(|e| e.base().subscription.is_some())
}

/// Keep a JSON-RPC subscription's (redacted) events for the live view.
/// Never returns: the runner holds the feed's sender until the send is over.
pub async fn collect(mut events: UnboundedReceiver<Value>, live: &LiveEvents) {
    let started = Instant::now();
    while let Some(payload) = events.recv().await {
        if let Ok(mut log) = live.lock() {
            log.push(LiveEvent {
                elapsed_ms: rustman_http::millis(started.elapsed()),
                payload,
            });
        }
    }
    std::future::pending::<()>().await;
}
