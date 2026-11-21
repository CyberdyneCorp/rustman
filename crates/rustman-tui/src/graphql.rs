//! The GraphQL actions of the workbench: loading and refreshing a tab's
//! schema, the completion popup, and starting and stopping subscriptions
//! (spec: TUI GraphQL pane). The editing itself is in
//! [`crate::graphql_pane`].

use rustman_core::model::GraphqlTransport;

use crate::app::{Effect, State};
use crate::graphql_pane::SchemaState;
use crate::graphql_schema::{SchemaJob, SchemaLoaded};
use crate::tabs::TabId;
use crate::toast::Severity;
use crate::{send, workbench};

/// Start loading the schema of every GraphQL tab that has not asked yet.
pub fn pending_loads(state: &mut State) -> Vec<Effect> {
    let waiting: Vec<TabId> = state
        .tabs
        .list
        .iter()
        .filter(|t| {
            t.editor
                .graphql()
                .is_some_and(|g| matches!(g.schema, SchemaState::Unloaded))
        })
        .map(|t| t.id)
        .collect();
    waiting
        .into_iter()
        .filter_map(|id| load(state, id, false))
        .collect()
}

/// `graphql.refresh_schema`: introspect the current tab's endpoint and
/// replace its cached schema.
pub fn refresh(state: &mut State) -> Vec<Effect> {
    let Some(id) = state.tabs.active().map(|t| t.id) else {
        return Vec::new();
    };
    match load(state, id, true) {
        Some(effect) => {
            state.notify(Severity::Info, "Refreshing the GraphQL schema…");
            vec![effect]
        }
        None => {
            not_graphql(state);
            Vec::new()
        }
    }
}

/// The load of tab `id`'s schema; `None` when it is not a GraphQL tab or
/// its request does not parse.
fn load(state: &mut State, id: TabId, refresh: bool) -> Option<Effect> {
    let workspace = workbench::workspace(state).cloned().map(Box::new);
    let environment = state.status.environment.clone();
    let backend = state.secret_backend;
    let tab = state.tabs.get_mut(id)?;
    let request = tab.editor.request().ok();
    let editor = tab.editor.graphql_mut()?;
    let Some(request) = request else {
        editor.schema = SchemaState::Failed("the request does not parse".to_owned());
        return None;
    };
    editor.schema = SchemaState::Loading;
    Some(Effect::GraphqlSchema(Box::new(SchemaJob {
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
    result: Result<Box<SchemaLoaded>, String>,
) {
    let Some(editor) = state.tabs.get_mut(id).and_then(|t| t.editor.graphql_mut()) else {
        return;
    };
    match result {
        Ok(loaded) => {
            let SchemaLoaded {
                state: schema,
                document,
            } = *loaded;
            editor.schema_loaded(schema, document);
            if refresh {
                let types = editor.loaded().map_or(0, |l| l.schema.type_count());
                state.notify(
                    Severity::Success,
                    format!("Schema refreshed: {types} types"),
                );
            }
        }
        Err(message) => {
            editor.schema_loaded(SchemaState::Failed(message.clone()), None);
            if refresh {
                state.notify(Severity::Error, format!("Schema not refreshed: {message}"));
            }
        }
    }
}

/// `editor.complete`: the completion popup at the document's cursor.
pub fn complete(state: &mut State) {
    let Some(editor) = state.tabs.active_mut().and_then(|t| t.editor.graphql_mut()) else {
        return not_graphql(state);
    };
    if editor.open_completion() {
        state.shell.focus = crate::focus::Focus::EditorBody;
        return;
    }
    let why = if editor.loaded().is_none() {
        "No completions: the schema is not loaded"
    } else {
        "No completions here"
    };
    state.notify(Severity::Info, why);
}

/// `graphql.subscription_start`: send a subscription. It runs until it
/// completes or is stopped.
pub fn start_subscription(state: &mut State) -> Vec<Effect> {
    let transport = state
        .tabs
        .active()
        .and_then(|t| t.editor.graphql())
        .map(|g| g.base().effective_transport());
    match transport {
        None => {
            not_graphql(state);
            Vec::new()
        }
        Some(GraphqlTransport::Http) => {
            let message = "Not a subscription: set transport to ws or sse (Ctrl+Enter sends it)";
            state.notify(Severity::Info, message);
            Vec::new()
        }
        Some(_) => send::start(state),
    }
}

/// `graphql.subscription_stop`: stop collecting; the events so far are
/// shown and recorded. False when no request is in flight.
pub fn stop_subscription(state: &mut State) -> bool {
    let flight = state.tabs.active().and_then(|t| t.in_flight.as_ref());
    let Some(flight) = flight else {
        state.notify(Severity::Info, "No subscription is running");
        return false;
    };
    flight.stop.cancel();
    true
}

fn not_graphql(state: &mut State) {
    state.notify(Severity::Info, "Open a GraphQL request first");
}

/// `graphql.display`: show the current GraphQL result's blockchain
/// scalars formatted, in token units or raw.
pub fn cycle_display(state: &mut State) {
    let view = state
        .tabs
        .active_mut()
        .and_then(|t| t.exchange.as_mut())
        .and_then(|r| r.graphql.as_mut());
    let shown = match view {
        Some(view) => match view.cycle_display() {
            true => format!("GraphQL values: {}", view.display.label()),
            false => "No blockchain preset: values are shown as received".to_owned(),
        },
        None => "No GraphQL result to format".to_owned(),
    };
    state.notify(Severity::Info, shown);
}
