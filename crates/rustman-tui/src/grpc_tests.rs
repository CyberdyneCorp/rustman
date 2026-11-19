//! The gRPC pane driven through `update` with keys, against a fixture
//! workspace and the in-process tonic test servers (add-grpc-client task
//! 6.2): the method browser and the template it seeds, offline browsing
//! from the descriptor cache, the status and details of a failed call, and
//! a bidi call that sends typed messages, half-closes and shows its
//! timeline filtered by direction and text. A secret is in the metadata of
//! every request, and no screen may show it.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::grpc::{GrpcServer, Reflection, UNREACHABLE_TARGET};

use crate::app::{Action, Effect, State, update};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::grpc_pane::{GrpcEditor, GrpcTab, SchemaState};
use crate::response::ResponseTab;
use crate::snapshot::{buffer_text, render};
use crate::tabs::{LiveEvent, LiveLog};
use crate::textarea::TextArea;
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press};

/// A secret value that must never be drawn.
const CANARY: &str = "canary-grpc-7a2c-token";

const METADATA: &str = "metadata:\n  - name: authorization\n    value: 'Bearer {{token}}'\n";

/// A workspace whose `local` environment points `grpc_host` at `target`,
/// with the secret `token` ([`CANARY`]) and the requests `grpc/get`,
/// `grpc/invalid` and `grpc/chat`, each sending the secret as metadata.
fn workspace(target: &str) -> tempfile::TempDir {
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
        &format!("variables:\n  grpc_host: '{target}'\nsecrets: [token]\n"),
    );
    let request = |name: &str, method: &str, body: &str| {
        format!(
            "version: 1\nkind: grpc\nname: {name}\ntarget: '{{{{grpc_host}}}}'\n\
             method: shop.v1.Catalog/{method}\n{body}{METADATA}"
        )
    };
    write(
        "grpc/get.rman.yaml",
        &request("Get", "GetProduct", "message: {}\n"),
    );
    write(
        "grpc/invalid.rman.yaml",
        &request("Invalid", "GetProduct", "message: {id: invalid}\n"),
    );
    write(
        "grpc/chat.rman.yaml",
        &request("Chat", "Chat", "messages:\n  - {text: hi}\n"),
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    tmp
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

/// The workspace in a `width` x `height` shell.
async fn shell(tmp: &tempfile::TempDir, width: u16, height: u16) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::TrueColor,
    );
    settle(&mut state, effects).await;
    state.secret_backend = Some(SecretBackend::File);
    update(&mut state, Action::Resize { width, height });
    state
}

/// Open the request at `path`; the update that follows loads its schema.
async fn open(state: &mut State, path: &str) {
    crate::workbench::open_path(state, Path::new(path));
    let effects = update(state, Action::Tick(state.now));
    settle(state, effects).await;
}

/// The screen without toasts, which come and go with time.
fn text(state: &mut State) -> String {
    state.notifications = crate::toast::Notifications::default();
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn editor(state: &State) -> &GrpcEditor {
    state.tabs.active().unwrap().editor.grpc().unwrap()
}

fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        update(state, Action::Key(key));
    }
}

/// Fix what changes from run to run: the duration and the `date` header.
fn fix_varying(received: &mut crate::response::Received) {
    received.record.duration_ms = 12.0;
    let view = received.grpc.as_mut().unwrap();
    view.headers.retain(|(name, _)| name != "date");
    view.trailers.retain(|(name, _)| name != "date");
}

/// Wait up to 20 s for `count` to reach `n`.
async fn wait_for(count: &impl Fn() -> usize, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while count() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{n} timeline entries within 20 s, got {}",
            count()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_method_browser_lists_methods_and_seeds_the_template() {
    let server = GrpcServer::start(Reflection::V1).await;
    let tmp = workspace(&server.target());
    let mut state = shell(&tmp, 160, 30).await;
    open(&mut state, "grpc/get.rman.yaml").await;
    let SchemaState::Ready(loaded) = &editor(&state).schema else {
        panic!("no schema: {:?}", editor(&state).schema);
    };
    assert_eq!(loaded.methods.len(), 5, "reflection is a service too");
    state.shell.focus = Focus::EditorBody;
    let browser = text(&mut state);
    assert!(
        browser.contains("Schema: 5 methods by reflection"),
        "{browser}"
    );
    assert!(browser.contains("shop.v1.Catalog/Chat"), "{browser}");
    assert!(!browser.contains(CANARY), "{browser}");
    insta::assert_snapshot!("grpc_method_browser", browser);
    press(&mut state, "Down Enter");
    assert_eq!(editor(&state).method, "shop.v1.Catalog/ListProducts");
    state.shell.focus = Focus::EditorTabs;
    press(&mut state, "2");
    assert_eq!(editor(&state).tab, GrpcTab::Message);
    let message = text(&mut state);
    assert!(
        message.contains("Message (JSON) shop.v1.ListProductsRequest"),
        "{message}"
    );
    insta::assert_snapshot!("grpc_message_template", message);
    press(&mut state, "3");
    let metadata = text(&mut state);
    assert!(metadata.contains("Bearer {{token}}"), "{metadata}");
    assert!(!metadata.contains(CANARY), "{metadata}");
}

/// Cache the schema `live` serves under [`UNREACHABLE_TARGET`] in the
/// workspace at `root`.
async fn cache_offline(root: &Path, live: &str) {
    use rustman_grpc::{CachePolicy, DescriptorCache, Remote, reflect_schema};
    let cache = DescriptorCache::for_workspace(root);
    let live = Remote::parse(live).unwrap();
    let policy = CachePolicy {
        cache: Some(cache.clone()),
        refresh: true,
        redactor: rustman_core::Redactor::new(),
    };
    reflect_schema(&live, &policy).await.unwrap();
    let (pool, _) = cache.read(&live.key()).unwrap();
    let offline = Remote::parse(UNREACHABLE_TARGET).unwrap();
    let redactor = rustman_core::Redactor::new();
    cache.write(&offline.key(), &pool, &redactor).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_schema_is_browsed_offline_with_a_stale_notice() {
    // Scenario "Offline browsing": the workspace's target cannot be
    // reached, and the cache holds what a live server served.
    let server = GrpcServer::start(Reflection::V1).await;
    let tmp = workspace(UNREACHABLE_TARGET);
    cache_offline(&tmp.path().join("ws"), &server.target()).await;
    drop(server);
    let mut state = shell(&tmp, 160, 30).await;
    open(&mut state, "grpc/get.rman.yaml").await;
    assert!(editor(&state).loaded().is_some());
    let effects = update(&mut state, Action::Key(event("Ctrl+G")));
    settle(&mut state, effects).await;
    let loaded = editor(&state).loaded().expect("the cache stands in");
    assert!(loaded.stale);
    assert_eq!(loaded.methods.len(), 5, "reflection is a service too");
    let screen = text(&mut state);
    assert!(
        screen.contains("Schema (may be stale, Ctrl+G refreshes): 5 methods"),
        "{screen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_call_shows_its_status_details_and_metadata() {
    let server = GrpcServer::start(Reflection::V1).await;
    let tmp = workspace(&server.target());
    let mut state = shell(&tmp, 160, 30).await;
    open(&mut state, "grpc/invalid.rman.yaml").await;
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    let received = &server.calls()[0];
    assert_eq!(
        received.text("authorization").as_deref(),
        Some(format!("Bearer {CANARY}").as_str()),
        "the secret is sent"
    );
    let tab = state.tabs.active_mut().unwrap();
    assert_eq!(tab.viewer.tab, ResponseTab::Messages);
    fix_varying(tab.exchange.as_mut().unwrap());
    state.shell.focus = Focus::Response;
    press(&mut state, "]");
    let status = text(&mut state);
    assert!(
        status.contains("INVALID_ARGUMENT (3)  invalid product id"),
        "{status}"
    );
    assert!(status.contains("google.rpc.BadRequest"), "{status}");
    assert!(!status.contains(CANARY), "{status}");
    insta::assert_snapshot!("grpc_status_details", status);
    for _ in 0..3 {
        press(&mut state, "]");
        assert!(!text(&mut state).contains(CANARY));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bidi_call_sends_typed_messages_half_closes_and_filters_its_timeline() {
    let server = GrpcServer::start(Reflection::V1).await;
    let tmp = workspace(&server.target());
    let mut state = shell(&tmp, 160, 30).await;
    open(&mut state, "grpc/chat.rman.yaml").await;
    assert!(editor(&state).is_bidi());
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Ctrl+R sends: {effects:?}");
    };
    let job = job.clone();
    let running = tokio::task::spawn_blocking(move || crate::send::execute(&job));
    let flight = state.tabs.active().unwrap().in_flight.as_ref().unwrap();
    let live = flight.live.clone();
    let count = || live.lock().unwrap().total();
    // The file's message, the welcome and its echo.
    wait_for(&count, 3).await;
    let editor = state.tabs.active_mut().unwrap().editor.grpc_mut().unwrap();
    editor.message = TextArea::from_text("{\"text\": \"more\"}");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    assert!(effects.is_empty(), "the message goes on the open stream");
    wait_for(&count, 5).await;
    // Elapsed times change from run to run: fix them for the snapshot.
    let (_, events) = live.lock().unwrap().newest(5);
    let mut steady = LiveLog::default();
    for (n, event) in events.into_iter().enumerate() {
        steady.push(LiveEvent {
            elapsed_ms: 10.0 * (n as f64 + 1.0),
            payload: event.payload,
        });
    }
    *live.lock().unwrap() = steady;
    state.shell.focus = Focus::Response;
    let screen = text(&mut state);
    assert!(screen.contains("5 messages"), "{screen}");
    assert!(screen.contains("echo: more"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
    insta::assert_snapshot!("grpc_live_timeline", screen);
    let effects = update(&mut state, Action::Key(event("Alt+X")));
    assert!(effects.is_empty());
    let done = running.await.unwrap();
    update(&mut state, done);
    let tab = state.tabs.active_mut().unwrap();
    assert!(tab.in_flight.is_none(), "half-closing ends the call");
    let received = tab.exchange.as_mut().unwrap();
    fix_varying(received);
    let view = received.grpc.as_mut().unwrap();
    assert_eq!(view.summary, "bidi shop.v1.Catalog/Chat · OK (0)");
    assert_eq!(view.timeline.len(), 5);
    for (n, row) in view.timeline.iter_mut().enumerate() {
        row.at_ms = n as f64 + 0.5;
    }
    press(&mut state, "d");
    let sent = text(&mut state);
    assert!(sent.contains("Direction: sent (d)"), "{sent}");
    assert!(sent.contains("2 of 5"), "{sent}");
    press(&mut state, "d f");
    type_text(&mut state, "more");
    let filtered = text(&mut state);
    assert!(filtered.contains("Direction: received (d)"), "{filtered}");
    assert!(filtered.contains("1 of 5"), "{filtered}");
    assert!(!filtered.contains(CANARY), "{filtered}");
    insta::assert_snapshot!("grpc_timeline_filtered", filtered);
}
