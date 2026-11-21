//! The messaging pane driven through `update` with keys, against a fixture
//! workspace and the in-process MQTT broker (add-messaging-client task
//! 6.2): the pane for `kafka`, `mqtt` and `amqp` requests, the broker and
//! destination form, the produce composer, a live consumer whose messages
//! arrive redacted, the pause, the value rendering (JSON, Schema Registry,
//! hex) and the 10000-message buffer. A secret is in the consumed message,
//! and no screen may show it.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use rustman_core::model::Request;
use rustman_core::{Secret, SecretBackend, SecretStore};
use serde_json::{Value, json};

use crate::app::{Action, Effect, State, update};
use crate::config::Overrides;
use crate::focus::Focus;
use crate::messaging::{BUFFER_BYTES, BUFFER_CAP};
use crate::messaging_pane::{MessagingEditor, MsgTab};
use crate::snapshot::{buffer_text, render};
use crate::textarea::TextArea;
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press, run};
use rustman_test_support::mqtt_broker::{Mode, MqttBroker};

/// A secret value that must never be drawn.
const CANARY: &str = "canary-messaging-5e07-pw";

/// A workspace whose `local` environment points `mqtt` at the broker,
/// with the secret `mqtt_pw` ([`CANARY`]), a Kafka consume, an MQTT
/// consume and an MQTT produce that sends the secret.
fn workspace(mqtt: &str) -> tempfile::TempDir {
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
        &format!("variables:\n  mqtt: '{mqtt}'\nsecrets: [mqtt_pw]\n"),
    );
    write(
        "events/orders.rman.yaml",
        "version: 1\nkind: kafka\nname: Orders\noperation: consume\nbrokers: [localhost:9092, localhost:9093]\ntopic: orders\noffset: earliest\n",
    );
    write(
        "events/temps.rman.yaml",
        "version: 1\nkind: mqtt\nname: Temps\noperation: consume\nbroker: '{{mqtt}}'\nauth: {username: app, password: '{{mqtt_pw}}'}\ntopic: 'sensors/#'\n",
    );
    write(
        "events/publish.rman.yaml",
        "version: 1\nkind: mqtt\nname: Publish\noperation: produce\nbroker: '{{mqtt}}'\nauth: {username: app, password: '{{mqtt_pw}}'}\ntopic: sensors/room1\nqos: 1\nmessages:\n- value: {temp: 21.5, token: '{{mqtt_pw}}'}\n",
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("mqtt_pw", CANARY.to_owned()))
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

fn open(state: &mut State, path: &str) {
    crate::workbench::open_path(state, Path::new(path));
    let effects = update(state, Action::Tick(state.now));
    run(state, effects);
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn editor(state: &State) -> &MessagingEditor {
    state.tabs.active().unwrap().editor.messaging().unwrap()
}

fn editor_mut(state: &mut State) -> &mut MessagingEditor {
    state
        .tabs
        .active_mut()
        .unwrap()
        .editor
        .messaging_mut()
        .unwrap()
}

fn push(state: &State, messages: impl IntoIterator<Item = Value>) {
    let editor = editor(state);
    let mut log = editor.consumer.log();
    for message in messages {
        log.push(message);
    }
}

#[test]
fn the_pane_says_when_the_oldest_messages_were_dropped() {
    let tmp = workspace("mqtt://127.0.0.1:9");
    let mut state = shell(&tmp, 140, 24);
    open(&mut state, "events/orders.rman.yaml");
    let big = |n: usize| json!({"offset": n, "value": "x".repeat(BUFFER_BYTES / 4)});
    push(&state, (0..6).map(big));
    let screen = text(&state);
    assert!(
        screen.contains("3 messages · 3 oldest dropped (over 64 MiB)"),
        "{screen}"
    );
}

#[test]
fn messaging_requests_open_in_the_messaging_pane() {
    let tmp = workspace("mqtt://127.0.0.1:9");
    let mut state = shell(&tmp, 110, 24);
    open(&mut state, "events/orders.rman.yaml");
    let tab = state.tabs.active().unwrap();
    assert_eq!(
        tab.editor.tabs(),
        ["Messages", "Compose", "Broker", "Assertions"]
    );
    assert_eq!(tab.editor.method(), Some("CONSUME"));
    assert_eq!(
        tab.editor.url().unwrap().text(),
        "localhost:9092,localhost:9093"
    );
    let screen = text(&state);
    assert!(
        screen.contains("No messages yet. Ctrl+Enter starts consuming."),
        "{screen}"
    );
    insta::assert_snapshot!("messaging_idle", screen);
    state.shell.focus = Focus::EditorBody;
    editor_mut(&mut state).tab = MsgTab::Broker;
    let screen = text(&state);
    assert!(screen.contains("topic: orders"), "{screen}");
    assert!(screen.contains("offset: earliest"), "{screen}");
    insta::assert_snapshot!("messaging_broker_form", screen);
}

#[test]
fn consumed_values_render_as_json_schema_registry_text_and_hex() {
    let tmp = workspace("mqtt://127.0.0.1:9");
    let mut state = shell(&tmp, 110, 30);
    open(&mut state, "events/orders.rman.yaml");
    push(
        &state,
        [
            json!({"partition": 0, "offset": 0, "key": "order-1", "headers": [], "value": {"id": 1, "type": "OrderCreated"}, "size": 33}),
            json!({"partition": 0, "offset": 1, "key": null, "headers": [], "value": {"id": 2, "total": 9.5}, "size": 18, "schema_id": 7}),
            json!({"partition": 1, "offset": 0, "key": null, "headers": [], "value": "plain text", "size": 10}),
            json!({"partition": 1, "offset": 1, "key": null, "headers": [], "value": {"base64": "AAEC/2hlbGxvIHdvcmxkIQ=="}, "size": 16}),
        ],
    );
    state.shell.focus = Focus::EditorBody;
    let screen = text(&state);
    assert!(
        screen.contains("Value (binary, 16 B)"),
        "follows the newest: {screen}"
    );
    assert!(screen.contains("00 01 02 ff 68 65 6c 6c"), "{screen}");
    insta::assert_snapshot!("messaging_hex", screen);
    press(&mut state, "k k");
    assert_eq!(editor(&state).selected, Some(1));
    let screen = text(&state);
    assert!(
        screen.contains("Value (decoded from Schema Registry id 7, 18 B)"),
        "{screen}"
    );
    assert!(screen.contains("\"total\": 9.5"), "{screen}");
    insta::assert_snapshot!("messaging_schema_registry", screen);
}

#[test]
fn the_pause_freezes_the_list_and_a_full_buffer_stops_it() {
    let tmp = workspace("mqtt://127.0.0.1:9");
    let mut state = shell(&tmp, 110, 24);
    open(&mut state, "events/orders.rman.yaml");
    let message = |n: usize| json!({"partition": 0, "offset": n, "headers": [], "value": {"n": n}, "size": 8});
    push(&state, (0..3).map(message));
    state.shell.focus = Focus::EditorBody;
    press(&mut state, "p");
    push(&state, (3..5).map(message));
    let screen = text(&state);
    assert!(
        screen.contains("Paused · 3 messages (2 more since the pause)"),
        "{screen}"
    );
    press(&mut state, "p");
    push(&state, (5..BUFFER_CAP + 10).map(message));
    let started = Instant::now();
    let screen = text(&state);
    assert_eq!(editor(&state).consumer.log().len(), BUFFER_CAP);
    assert!(screen.contains("Buffer full · 10000 messages"), "{screen}");
    assert!(
        screen.contains("{\"n\":9999}"),
        "follows the newest: {screen}"
    );
    for _ in 0..20 {
        press(&mut state, "k");
        let _ = text(&state);
    }
    assert_eq!(
        editor(&state).selected,
        Some(9_979),
        "every key was handled"
    );
    assert!(started.elapsed() < Duration::from_secs(60));
    insta::assert_snapshot!("messaging_full", text(&state));
}

#[test]
fn the_composer_edits_the_messages_of_a_produce() {
    let tmp = workspace("mqtt://127.0.0.1:9");
    let mut state = shell(&tmp, 110, 24);
    open(&mut state, "events/publish.rman.yaml");
    assert_eq!(
        state.tabs.active().unwrap().editor.method(),
        Some("PRODUCE")
    );
    state.shell.focus = Focus::EditorBody;
    editor_mut(&mut state).tab = MsgTab::Compose;
    let screen = text(&state);
    assert!(screen.contains("value:"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
    insta::assert_snapshot!("messaging_compose", screen);
    let editor = editor_mut(&mut state);
    editor.compose = TextArea::from_text("- value: hello\n- {value: {n: 2}}");
    let Request::Messaging(edited) = editor.to_request().unwrap() else {
        panic!("not messaging");
    };
    assert_eq!(edited.messages.as_ref().unwrap().len(), 2);
    let at = |key: &str| edited.source.find(key).unwrap_or_else(|| panic!("{key}"));
    assert!(at("broker:") < at("topic:") && at("topic:") < at("messages:"));
    editor.compose = TextArea::from_text("");
    assert!(
        editor.to_request().unwrap_err().contains("messages"),
        "a produce needs messages"
    );
}

/// Wait until the active tab's consumer holds `n` messages.
async fn wait_for_messages(state: &State, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while editor(state).consumer.log().len() < n {
        assert!(Instant::now() < deadline, "{n} messages within 20 s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_consumer_shows_redacted_messages_and_stops_with_esc() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", CANARY);
    let tmp = workspace(&broker.url());
    let mut state = shell(&tmp, 140, 30);
    open(&mut state, "events/temps.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Ctrl+R starts consuming: {effects:?}");
    };
    let job = job.clone();
    let running = tokio::task::spawn_blocking(move || crate::send::execute(&job));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !broker.log().iter().any(|l| l.starts_with("SUBSCRIBE")) {
        assert!(Instant::now() < deadline, "{:?}", broker.log());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(editor(&state).consumer.is_running());

    // Produce from a second tab: the message carries the secret.
    open(&mut state, "events/publish.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    let [Effect::Send(produce)] = effects.as_slice() else {
        panic!("Ctrl+R produces: {effects:?}");
    };
    let produce = produce.clone();
    let done = tokio::task::spawn_blocking(move || crate::send::execute(&produce))
        .await
        .unwrap();
    update(&mut state, done);
    open(&mut state, "events/temps.rman.yaml");
    wait_for_messages(&state, 1).await;
    let shown = editor(&state).consumer.log().get(0).cloned().unwrap();
    assert_eq!(shown["value"]["token"], "«redacted:mqtt_pw»");
    let screen = text(&state);
    assert!(screen.contains("Consuming · 1 message"), "{screen}");
    assert!(screen.contains("«redacted:mqtt_pw»"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");

    state.shell.focus = Focus::Response;
    let effects = update(&mut state, Action::Key(event("Esc")));
    assert!(effects.is_empty());
    let done = running.await.unwrap();
    update(&mut state, done);
    let tab = state.tabs.active().unwrap();
    assert!(tab.in_flight.is_none());
    let record = &tab.exchange.as_ref().unwrap().record;
    let messaging = record.messaging.as_ref().unwrap();
    let recorded = serde_json::to_string(messaging).unwrap();
    assert!(recorded.contains("sensors/room1"), "{recorded}");
    assert!(!recorded.contains(CANARY), "{recorded}");
    let screen = text(&state);
    assert!(screen.contains("Stopped · 1 message"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
}
