//! The JSON-RPC pane driven through `update` with keys, against a fixture
//! workspace and the in-process JSON-RPC test servers (add-jsonrpc-client
//! tasks 7.1 and 7.3): catalog completion, a call's result, a live
//! subscription stopped (and unsubscribed) with `Esc`, and a method guard
//! refusing a send. A secret is in the params of every request, and no
//! screen may show it.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustman_core::run::RpcRefusal;
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::{
    JsonrpcHttpServer, JsonrpcWsServer, RecordedRequest, RpcReply, WsFrame,
};
use serde_json::{Value, json};

use crate::app::{Action, Effect, State, update};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::jsonrpc_pane::{CallField, CatalogState, JsonrpcEditor};
use crate::response::ResponseTab;
use crate::snapshot::{buffer_text, render};
use crate::tabs::{LiveEvent, LiveLog};
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press, run};

/// A secret value that must never be drawn.
const CANARY: &str = "canary-rpc-5e1d-key";

const OPENRPC: &str = r#"{"openrpc": "1.2.6", "methods": [
  {"name": "eth_blockNumber", "params": []},
  {"name": "eth_getBalance", "params": [
    {"name": "address", "required": true, "schema": {"type": "string"}},
    {"name": "block", "schema": {"type": "string"}}]},
  {"name": "eth_getCode", "params": [
    {"name": "address", "required": true, "schema": {"type": "string"}}]},
  {"name": "eth_subscribe", "params": []}
]}"#;

/// A workspace whose `local` environment points `http` and `ws` at the
/// servers, with the secret `api_key` ([`CANARY`]), a catalog linked to
/// `{{http}}`, and the jsonrpc requests `head` (a call), `heads` (a
/// subscription) and `guarded` (a method a test guard refuses).
fn workspace(http: &str, ws: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n\n\
         [jsonrpc.catalogs]\n\"{{http}}\" = \"openrpc.json\"\n",
    );
    write(
        "environments/local.yaml",
        &format!("variables:\n  http: {http}\n  ws: {ws}\nsecrets: [api_key]\n"),
    );
    write("openrpc.json", OPENRPC);
    write(
        "chain/head.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Head\nendpoint: '{{http}}'\nmethod: eth_blockNumber\n\
         params: ['{{api_key}}']\n",
    );
    write(
        "chain/heads.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Heads\nendpoint: '{{ws}}'\n\
         params: [newHeads, '{{api_key}}']\nsubscription:\n  subscribe: eth_subscribe\n\
         \x20 unsubscribe: eth_unsubscribe\n  notification: eth_subscription\n",
    );
    write(
        "chain/guarded.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Guarded\nendpoint: '{{http}}'\n\
         method: tui_guarded_send\nparams: ['{{api_key}}']\n",
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("api_key", CANARY.to_owned()))
        .unwrap();
    tmp
}

/// The workspace in a `width` x `height` shell.
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

/// Open the request at `path`; the update that follows finds its catalog.
fn open(state: &mut State, path: &str) {
    crate::workbench::open_path(state, Path::new(path));
    let effects = update(state, Action::Tick(state.now));
    run(state, effects);
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn editor(state: &State) -> &JsonrpcEditor {
    state.tabs.active().unwrap().editor.jsonrpc().unwrap()
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

/// An EVM-like node over HTTP.
fn node(hit: &RecordedRequest, _: usize) -> RpcReply {
    let call: Value = serde_json::from_slice(&hit.body).unwrap();
    RpcReply::json(&json!({"jsonrpc": "2.0", "id": call["id"], "result": "0x10"}))
}

#[test]
fn typing_a_method_prefix_suggests_catalog_methods_with_their_params() {
    // Scenario "Method completion".
    let tmp = workspace("http://127.0.0.1:9", "ws://127.0.0.1:9");
    let mut state = shell(&tmp, 100, 24);
    open(&mut state, "chain/head.rman.yaml");
    let CatalogState::Ready(Some(catalog)) = &editor(&state).catalog else {
        panic!("no catalog: {:?}", editor(&state).catalog);
    };
    assert_eq!(catalog.methods.len(), 4);
    state.shell.focus = Focus::EditorBody;
    assert_eq!(editor(&state).field, CallField::Method);
    press(&mut state, "End");
    for _ in 0.."eth_blockNumber".len() {
        press(&mut state, "Backspace");
    }
    type_text(&mut state, "eth_get");
    let screen = text(&state);
    assert!(
        screen.contains(" eth_getBalance(address, block?) "),
        "{screen}"
    );
    assert!(screen.contains(" eth_getCode(address) "), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
    insta::assert_snapshot!("jsonrpc_method_completion", screen);
    press(&mut state, "Enter");
    assert_eq!(editor(&state).method.text(), "eth_getBalance");
    press(&mut state, "Ctrl+Space");
    assert!(editor(&state).popup.is_some(), "Ctrl+Space opens it again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_shows_its_result_as_received() {
    let server = JsonrpcHttpServer::start(node).await;
    let tmp = workspace(&server.url("/"), "ws://127.0.0.1:9");
    let mut state = shell(&tmp, 100, 24);
    open(&mut state, "chain/head.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    let hit: Value = serde_json::from_slice(&server.hits()[0].body).unwrap();
    assert_eq!(hit["params"][0], CANARY, "the secret is sent");
    let tab = state.tabs.active().unwrap();
    assert_eq!(tab.viewer.tab, ResponseTab::Result);
    state.shell.focus = Focus::Response;
    let result = text(&state);
    assert!(result.contains("call eth_blockNumber · result"), "{result}");
    assert!(
        result.contains("\"0x10\""),
        "hex shown as received: {result}"
    );
    press(&mut state, "]");
    assert!(text(&state).contains("No errors."));
    for _ in 0..4 {
        press(&mut state, "]");
        assert!(!text(&state).contains(CANARY));
    }
}

/// A node that answers `eth_subscribe` with `0xs` and 3 `newHeads`
/// events, each echoing the subscribe params (a secret among them).
fn heads(message: &Value) -> Vec<WsFrame> {
    let id = message["id"].clone();
    if message["method"] != "eth_subscribe" {
        return vec![WsFrame::Json(
            json!({"jsonrpc": "2.0", "id": id, "result": true}),
        )];
    }
    let mut frames = vec![WsFrame::Json(
        json!({"jsonrpc": "2.0", "id": id, "result": "0xs"}),
    )];
    for n in 1..=3 {
        frames.push(WsFrame::Pause(Duration::from_millis(5)));
        frames.push(WsFrame::Json(json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {"subscription": "0xs", "result": {"number": n, "echo": message["params"]}}
        })));
    }
    frames
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_subscription_lists_events_until_esc_unsubscribes() {
    // Scenario "Live subscription".
    let server = JsonrpcWsServer::start(heads).await;
    let tmp = workspace("http://127.0.0.1:9", &server.url("/"));
    let mut state = shell(&tmp, 140, 24);
    open(&mut state, "chain/heads.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Ctrl+R sends: {effects:?}");
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
    let (_, events) = flight.newest_live(3);
    let payloads = serde_json::to_string(&events.iter().map(|e| &e.payload).collect::<Vec<_>>());
    let payloads = payloads.unwrap();
    assert!(payloads.contains("«redacted:api_key»"), "{payloads}");
    assert!(!payloads.contains(CANARY), "{payloads}");
    // Elapsed times change from run to run: fix them for the snapshot.
    let mut steady = LiveLog::default();
    for (n, event) in events.into_iter().enumerate() {
        steady.push(LiveEvent {
            elapsed_ms: 10.0 * (n as f64 + 1.0),
            payload: event.payload,
        });
    }
    *flight.live.lock().unwrap() = steady;
    state.shell.focus = Focus::Response;
    let live = text(&state);
    assert!(live.contains("Subscribed: 3 events"), "{live}");
    assert!(live.contains("(Esc unsubscribes)"), "{live}");
    assert!(!live.contains(CANARY), "{live}");
    insta::assert_snapshot!("jsonrpc_live_events", live);
    let effects = update(&mut state, Action::Key(event("Esc")));
    assert!(effects.is_empty());
    let done = running.await.unwrap();
    update(&mut state, done);
    assert_eq!(
        server.sessions()[0].methods(),
        ["eth_subscribe", "eth_unsubscribe"],
        "Esc unsubscribes"
    );
    let tab = state.tabs.active().unwrap();
    assert!(tab.in_flight.is_none());
    let record = tab
        .exchange
        .as_ref()
        .unwrap()
        .record
        .jsonrpc
        .clone()
        .unwrap();
    let subscription = record.subscription.unwrap();
    assert_eq!(subscription.ended, "stopped");
    assert!(subscription.unsubscribed);
    state.shell.focus = Focus::Response;
    press(&mut state, "] ]");
    let events = text(&state);
    assert!(events.contains("Ended: stopped, 3 events"), "{events}");
    assert!(!events.contains(CANARY), "{events}");
}

/// Refuses `tui_guarded_send`, as a capability's guard would.
struct TuiGuard;

impl rustman_jsonrpc::MethodGuard for TuiGuard {
    fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
        (method == "tui_guarded_send").then(|| "tui_guarded_send is guarded".to_owned())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guard_refuses_the_send_action() {
    // Task 7.3, TUI face.
    let guard: Arc<dyn rustman_jsonrpc::MethodGuard> = Arc::new(TuiGuard);
    rustman_jsonrpc::register_guard(guard.clone());
    let server = JsonrpcHttpServer::start(node).await;
    let tmp = workspace(&server.url("/"), "ws://127.0.0.1:9");
    let mut state = shell(&tmp, 100, 24);
    open(&mut state, "chain/guarded.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    rustman_jsonrpc::unregister_guard(&guard);
    assert!(server.hits().is_empty(), "nothing is sent");
    let record = &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .record;
    assert_eq!(record.error.as_deref(), Some("tui_guarded_send is guarded"));
    let refusal = record.jsonrpc.as_ref().and_then(|j| j.refusal);
    assert_eq!(refusal, Some(RpcRefusal::Guard));
    let screen = text(&state);
    assert!(screen.contains("tui_guarded_send is guarded"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
}

#[test]
fn the_help_overlay_names_method_completion() {
    let tmp = workspace("http://127.0.0.1:9/", "ws://127.0.0.1:9");
    let mut state = shell(&tmp, 100, 70);
    open(&mut state, "chain/head.rman.yaml");
    state.shell.focus = Focus::EditorTabs;
    press(&mut state, "?");
    assert!(matches!(
        state.overlay,
        Some(crate::app::Overlay::Help { .. })
    ));
    let screen = text(&state);
    let line = "Ctrl+Space               Show completions";
    assert!(screen.contains(line), "{line}\n{screen}");
    insta::assert_snapshot!("jsonrpc_help", screen);
}
