//! The GraphQL pane driven through `update` with keys, against a fixture
//! workspace and the in-process GraphQL test servers (add-graphql-client
//! tasks 7.1 and 7.2): inline diagnostics, completion, the explorer, the
//! result tabs, subscriptions started and stopped, the schema refresh and
//! the help overlay.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::{
    GraphqlHttpServer, GraphqlServer, GraphqlSseServer, Reply, SseEnd, SseScript,
};
use serde_json::json;

use crate::app::{Action, Effect, Overlay, State, update};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::graphql_pane::{GqlTab, SchemaState};
use crate::response::ResponseTab;
use crate::snapshot::{buffer_text, render, style_of};
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press, run};

/// A secret value that must never be drawn.
const CANARY: &str = "canary-gql-7f3a-token";

const SDL: &str = "type Query { user(id: ID!): User me(key: String): User }\n\
type Subscription { onMessage: Message }\ntype Message { n: Int }\n\
type User { id: ID! name: String email: String @deprecated(reason: \"use contact\") \
contact: String posts: [Post!]! }\ntype Post { title: String }\n";

/// Where the fixture's requests point.
struct Urls {
    http: String,
    sse: String,
    introspect: String,
}

impl Default for Urls {
    fn default() -> Self {
        let nowhere = "http://127.0.0.1:9".to_owned();
        Self {
            http: nowhere.clone(),
            sse: nowhere.clone(),
            introspect: nowhere,
        }
    }
}

/// A workspace with `schema.graphql`, the graphql requests `users`
/// (validated against it), `me` (its variables hold the secret `token`),
/// `live` (a subscription over SSE) and `remote` (no schema file), and the
/// environment `local` whose secret `token` is [`CANARY`].
fn workspace(urls: &Urls) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n",
    );
    write(
        "environments/local.yaml",
        &format!(
            "variables:\n  http: {}\n  sse: {}\n  introspect: {}\nsecrets: [token]\n",
            urls.http, urls.sse, urls.introspect
        ),
    );
    write("graph/schema.graphql", SDL);
    write(
        "graph/users.rman.yaml",
        "version: 1\nkind: graphql\nname: Users\nendpoint: '{{http}}/graphql'\n\
         schema_file: schema.graphql\ndocument: |-\n  { user(id: 1) {\n    name\n  } }\n",
    );
    write(
        "graph/me.rman.yaml",
        "version: 1\nkind: graphql\nname: Me\nendpoint: '{{http}}/graphql'\n\
         schema_file: schema.graphql\n\
         document: 'query Me($key: String) { me(key: $key) { name } }'\n\
         variables:\n  key: '{{token}}'\n",
    );
    write(
        "graph/live.rman.yaml",
        "version: 1\nkind: graphql\nname: Live\nendpoint: '{{sse}}/graphql'\ntransport: sse\n\
         document: 'subscription { onMessage { n } }'\n",
    );
    write(
        "graph/remote.rman.yaml",
        "version: 1\nkind: graphql\nname: Remote\nendpoint: '{{introspect}}/graphql'\n\
         document: '{ user(id: 1) { emial } }'\n",
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    tmp
}

/// The workspace in a `width` x `height` shell; effects run here, on this
/// thread (no server is involved).
fn shell(tmp: &tempfile::TempDir, width: u16, height: u16) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::TrueColor,
    );
    run(&mut state, effects);
    state.secret_backend = Some(SecretBackend::File);
    update(&mut state, Action::Resize { width, height });
    state
}

/// Open the request at `path`; the update that follows loads its schema.
fn open(state: &mut State, path: &str) -> Vec<Effect> {
    crate::workbench::open_path(state, Path::new(path));
    update(state, Action::Tick(state.now))
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn graphql(state: &State) -> &crate::graphql_pane::GraphqlEditor {
    state.tabs.active().unwrap().editor.graphql().unwrap()
}

/// Run effects (and those their completions return) on blocking threads:
/// they start runtimes of their own, which a test runtime cannot host.
async fn settle(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = tokio::task::spawn_blocking(move || execute(effect))
            .await
            .unwrap();
        effects.extend(update(state, done));
    }
}

fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let effects = update(state, Action::Key(key));
        run(state, effects);
    }
}

#[test]
fn typing_an_unknown_field_underlines_it_without_sending() {
    // Scenario "Inline diagnostic".
    let tmp = workspace(&Urls::default());
    let mut state = shell(&tmp, 120, 24);
    let effects = open(&mut state, "graph/users.rman.yaml");
    run(&mut state, effects);
    assert!(matches!(
        graphql(&state).schema,
        SchemaState::Ready(Some(_))
    ));
    press(&mut state, "Ctrl+B");
    state.shell.focus = Focus::EditorBody;
    press(&mut state, "Down End");
    type_text(&mut state, " emial");
    let tab = state.tabs.active().unwrap();
    assert!(
        tab.in_flight.is_none() && tab.exchange.is_none(),
        "nothing sent"
    );
    let buffer = render(&state, 120, 24);
    let screen = buffer_text(&buffer);
    assert!(
        screen.contains("2:8: Cannot query field \"emial\" on type \"User\"."),
        "{screen}"
    );
    let underlined = style_of(&buffer, "emial").unwrap();
    assert!(underlined.add_modifier.contains(Modifier::UNDERLINED));
    assert!(
        !style_of(&buffer, "name")
            .unwrap()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    insta::assert_snapshot!("graphql_inline_diagnostic", screen);
}

#[test]
fn completion_pops_up_and_inserts_the_chosen_field() {
    let tmp = workspace(&Urls::default());
    let mut state = shell(&tmp, 120, 24);
    let effects = open(&mut state, "graph/users.rman.yaml");
    run(&mut state, effects);
    state.shell.focus = Focus::EditorBody;
    press(&mut state, "Down End Enter");
    type_text(&mut state, "    p");
    press(&mut state, "Ctrl+Space");
    let screen = text(&state);
    assert!(screen.contains(" posts: [Post!]! "), "{screen}");
    insta::assert_snapshot!("graphql_completion_popup", screen);
    press(&mut state, "Enter");
    assert_eq!(
        graphql(&state).document.text(),
        "{ user(id: 1) {\n  name\n    posts\n} }"
    );
    assert!(graphql(&state).popup.is_none());
}

#[test]
fn the_explorer_lists_types_and_opens_one_with_its_deprecations() {
    let tmp = workspace(&Urls::default());
    let mut state = shell(&tmp, 120, 20);
    let effects = open(&mut state, "graph/users.rman.yaml");
    run(&mut state, effects);
    state.shell.focus = Focus::EditorTabs;
    press(&mut state, "3");
    assert_eq!(graphql(&state).tab, GqlTab::Schema);
    state.shell.focus = Focus::EditorBody;
    let screen = text(&state);
    assert!(
        screen.contains("query: Query  subscription: Subscription"),
        "{screen}"
    );
    let users = graphql(&state)
        .loaded()
        .unwrap()
        .overview
        .types
        .iter()
        .position(|t| t.name == "User")
        .unwrap();
    for _ in 0..users {
        press(&mut state, "Down");
    }
    press(&mut state, "Enter");
    let screen = text(&state);
    assert!(screen.contains("type User"), "{screen}");
    assert!(
        screen.contains("email: String  (deprecated: use contact)"),
        "{screen}"
    );
}

#[test]
fn a_document_with_errors_asks_before_sending() {
    let tmp = workspace(&Urls::default());
    let mut state = shell(&tmp, 120, 20);
    let effects = open(&mut state, "graph/users.rman.yaml");
    run(&mut state, effects);
    state.shell.focus = Focus::EditorBody;
    press(&mut state, "Down End");
    type_text(&mut state, " emial");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    assert!(effects.is_empty(), "{effects:?}");
    let Some(Overlay::Confirm(confirm)) = &state.overlay else {
        panic!("expected a confirmation, got {:?}", state.overlay);
    };
    assert_eq!(confirm.title, " Invalid GraphQL ");
    let effects = update(&mut state, Action::Key(event("Enter")));
    assert!(effects.is_empty(), "Cancel is preselected");
    assert!(state.tabs.active().unwrap().in_flight.is_none());
    update(&mut state, Action::Key(event("Ctrl+R")));
    let effects = update(&mut state, Action::Key(event("s")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Send anyway sends: {effects:?}");
    };
    assert!(!job.validate);
}

#[test]
fn the_help_overlay_lists_the_graphql_keys() {
    let tmp = workspace(&Urls::default());
    let mut state = shell(&tmp, 100, 70);
    let effects = open(&mut state, "graph/users.rman.yaml");
    run(&mut state, effects);
    state.shell.focus = Focus::EditorTabs;
    press(&mut state, "?");
    assert!(matches!(state.overlay, Some(Overlay::Help { .. })));
    let screen = text(&state);
    for line in [
        "Ctrl+Space               Show completions",
        "Ctrl+G                   Refresh GraphQL schema",
        "Alt+S                    Start GraphQL subscription",
        "Alt+X                    Stop GraphQL subscription",
    ] {
        assert!(screen.contains(line), "{line}\n{screen}");
    }
    insta::assert_snapshot!("graphql_help", screen);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_shows_data_and_errors_with_secrets_redacted() {
    let server = GraphqlHttpServer::start(|hit| {
        let key = hit.json()["variables"]["key"].clone();
        Reply::graphql(json!({
            "data": {"me": {"name": key}},
            "errors": [{"message": "private", "path": ["me", "email"],
                "extensions": {"code": "FORBIDDEN"}}],
            "extensions": {"cost": 3}
        }))
    })
    .await;
    let urls = Urls {
        http: server.url(""),
        ..Urls::default()
    };
    let tmp = workspace(&urls);
    let mut state = shell(&tmp, 120, 24);
    let effects = open(&mut state, "graph/me.rman.yaml");
    settle(&mut state, effects).await;
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    assert_eq!(server.hits()[0].json()["variables"]["key"], CANARY);
    let tab = state.tabs.active().unwrap();
    assert_eq!(tab.viewer.tab, ResponseTab::Data);
    state.shell.focus = Focus::Response;
    let data = text(&state);
    assert!(data.contains("\"name\": \"«redacted:token»\""), "{data}");
    press(&mut state, "]");
    let errors = text(&state);
    assert!(errors.contains("private"), "{errors}");
    assert!(
        errors.contains("path: me.email · code: FORBIDDEN"),
        "{errors}"
    );
    press(&mut state, "]");
    let extensions = text(&state);
    assert!(extensions.contains("\"cost\": 3"), "{extensions}");
    for screen in [data, errors, extensions] {
        assert!(!screen.contains(CANARY), "{screen}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_streams_events_until_stopped() {
    let server = GraphqlSseServer::start(SseScript {
        events: 3,
        end: SseEnd::Open,
    })
    .await;
    let urls = Urls {
        sse: server.url(""),
        ..Urls::default()
    };
    let tmp = workspace(&urls);
    let mut state = shell(&tmp, 120, 24);
    let effects = open(&mut state, "graph/live.rman.yaml");
    settle(&mut state, effects).await;
    let effects = update(&mut state, Action::Key(event("Alt+S")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Alt+S starts the subscription: {effects:?}");
    };
    let job = job.clone();
    let running = tokio::task::spawn_blocking(move || crate::send::execute(&job));
    let flight = state.tabs.active().unwrap().in_flight.clone().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while flight.live_count() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "3 events within 20 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let live = text(&state);
    assert!(live.contains("Subscribed: 3 events"), "{live}");
    // The feed row is clipped to the pane width, and the elapsed column
    // ("+12 ms" vs "+120 ms") moves the clip point: match a prefix that fits.
    assert!(
        live.contains("{\"data\":{\"onMessage\":{\"n\":3}"),
        "{live}"
    );
    let effects = update(&mut state, Action::Key(event("Alt+X")));
    assert!(effects.is_empty());
    let done = running.await.unwrap();
    update(&mut state, done);
    let tab = state.tabs.active().unwrap();
    assert!(tab.in_flight.is_none());
    let recorded = tab
        .exchange
        .as_ref()
        .unwrap()
        .record
        .graphql
        .as_ref()
        .unwrap();
    assert_eq!(recorded.subscription.as_ref().unwrap().ended, "stopped");
    state.shell.focus = Focus::Response;
    press(&mut state, "] ] ]");
    let events = text(&state);
    assert!(events.contains("Ended: stopped, 3 events"), "{events}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refreshing_the_schema_caches_it_and_validates_against_it() {
    let server = GraphqlServer::start(SDL).await;
    let urls = Urls {
        introspect: server.url(""),
        ..Urls::default()
    };
    let tmp = workspace(&urls);
    let mut state = shell(&tmp, 120, 20);
    let effects = open(&mut state, "graph/remote.rman.yaml");
    settle(&mut state, effects).await;
    assert!(matches!(graphql(&state).schema, SchemaState::Ready(None)));
    assert!(graphql(&state).validation.syntax_only);
    assert_eq!(server.introspection_hits(), 0, "opening never introspects");
    let effects = update(&mut state, Action::Key(event("Ctrl+G")));
    settle(&mut state, effects).await;
    assert_eq!(server.introspection_hits(), 1);
    let editor = graphql(&state);
    assert!(!editor.validation.syntax_only);
    assert!(editor.validation.has_errors());
    let screen = text(&state);
    assert!(screen.contains("Schema: cache, fetched"), "{screen}");
    let cache = tmp.path().join("ws/.rustman/graphql/schemas");
    assert_eq!(fs::read_dir(cache).unwrap().count(), 1);
}
