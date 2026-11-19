//! The realtime pane driven through `update` with keys, against a fixture
//! workspace and the in-process WebSocket test server (add-realtime-client
//! task 5.2): the pane for `ws` and `sse` requests, a live session whose
//! entries arrive redacted, a composed frame, the timeline filters, `Esc`
//! closing the session, and 10000 entries drawn without blocking input. A
//! secret is in the first frame of the session, and no screen may show it.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_realtime::{Direction, TimelineEntry};
use rustman_test_support::{RealtimeWsServer, WsBehavior};

use crate::app::{Action, Effect, State, update};
use crate::config::Overrides;
use crate::focus::Focus;
use crate::realtime::RealtimeLog;
use crate::realtime_pane::{RealtimeEditor, RtTab};
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press, run};

/// A secret value that must never be drawn.
const CANARY: &str = "canary-realtime-91b2-key";

/// A workspace whose `local` environment points `ws` at the server, with
/// the secret `api_key` ([`CANARY`]), the ws request `feeds/echo` (it sends
/// the secret first) and the sse request `feeds/events`.
fn workspace(ws: &str) -> tempfile::TempDir {
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
        &format!("variables:\n  ws: {ws}\nsecrets: [api_key]\n"),
    );
    write(
        "feeds/echo.rman.yaml",
        "version: 1\nkind: ws\nname: Echo\nurl: '{{ws}}'\nsteps:\n  - send: {text: 'token {{api_key}}'}\n",
    );
    write(
        "feeds/events.rman.yaml",
        "version: 1\nkind: sse\nname: Events\nurl: http://127.0.0.1:9/events\n",
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

fn open(state: &mut State, path: &str) {
    crate::workbench::open_path(state, Path::new(path));
    let effects = update(state, Action::Tick(state.now));
    run(state, effects);
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn editor(state: &State) -> &RealtimeEditor {
    state.tabs.active().unwrap().editor.realtime().unwrap()
}

fn editor_mut(state: &mut State) -> &mut RealtimeEditor {
    state
        .tabs
        .active_mut()
        .unwrap()
        .editor
        .realtime_mut()
        .unwrap()
}

fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let effects = update(state, Action::Key(key));
        run(state, effects);
    }
}

fn entry(dir: Direction, t_ms: u64, kind: &str, payload: &str) -> TimelineEntry {
    TimelineEntry {
        dir,
        t_ms,
        size: payload.len() as u64,
        kind: kind.to_owned(),
        payload: payload.to_owned(),
        id: None,
        rtt_ms: None,
    }
}

/// Wait until the active tab's log holds `n` entries.
async fn wait_for_entries(state: &State, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while editor(state).live.log().len() < n {
        assert!(Instant::now() < deadline, "{n} entries within 20 s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn ws_and_sse_requests_open_in_the_realtime_pane() {
    let tmp = workspace("ws://127.0.0.1:9/feed");
    let mut state = shell(&tmp, 100, 24);
    open(&mut state, "feeds/echo.rman.yaml");
    assert_eq!(
        state.tabs.active().unwrap().editor.tabs(),
        ["Timeline", "Compose", "Headers", "Steps", "Assertions"]
    );
    let screen = text(&state);
    assert!(screen.contains("Not connected"), "{screen}");
    assert!(
        screen.contains("No messages yet. Ctrl+R connects."),
        "{screen}"
    );
    assert!(!screen.contains(CANARY), "{screen}");
    insta::assert_snapshot!("realtime_idle", screen);
    open(&mut state, "feeds/events.rman.yaml");
    let tab = state.tabs.active().unwrap();
    assert_eq!(tab.editor.method(), Some("SSE"));
    assert_eq!(tab.editor.tabs()[3], "Body");
}

#[test]
fn edits_are_parsed_and_written_in_schema_order() {
    let tmp = workspace("ws://127.0.0.1:9/feed");
    let mut state = shell(&tmp, 100, 24);
    open(&mut state, "feeds/echo.rman.yaml");
    let editor = editor_mut(&mut state);
    editor.assertions = crate::textarea::TextArea::from_text("- messages: {count_gte: 1}");
    let rustman_core::model::Request::Ws(ws) = editor.to_request().unwrap() else {
        panic!("not ws");
    };
    let at = |key: &str| ws.source.find(key).unwrap_or_else(|| panic!("{key}"));
    assert!(at("url:") < at("steps:") && at("steps:") < at("assertions:"));
    editor.url = crate::textarea::TextArea::from_text("https://example.com/feed");
    assert!(
        editor
            .to_request()
            .unwrap_err()
            .contains("ws: url must use ws:// or wss://")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_shows_redacted_entries_sends_frames_and_closes_with_esc() {
    let server = RealtimeWsServer::start(WsBehavior::echo()).await;
    let tmp = workspace(&server.url("/feed"));
    let mut state = shell(&tmp, 180, 30);
    open(&mut state, "feeds/echo.rman.yaml");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Ctrl+R connects: {effects:?}");
    };
    let job = job.clone();
    let running = tokio::task::spawn_blocking(move || crate::send::execute(&job));
    // connect, the sent secret, its echo.
    wait_for_entries(&state, 3).await;
    let shown: Vec<String> = editor(&state)
        .live
        .entries(&[1, 2])
        .into_iter()
        .map(|e| e.payload)
        .collect();
    assert_eq!(shown, ["token «redacted:api_key»"; 2]);

    // Compose a frame and send it while connected.
    state.shell.focus = Focus::EditorBody;
    editor_mut(&mut state).tab = RtTab::Compose;
    type_text(&mut state, "hello trade");
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    assert!(effects.is_empty(), "a frame, not a new send");
    wait_for_entries(&state, 5).await;

    // Filter to received entries containing "trade".
    editor_mut(&mut state).tab = RtTab::Timeline;
    press(&mut state, "d d");
    press(&mut state, "/");
    type_text(&mut state, "trade");
    press(&mut state, "Enter");
    let filter = editor(&state).filter.clone();
    assert_eq!(filter.dir, Some(Direction::Received));
    assert_eq!(filter.text, "trade");
    let matching = editor(&state).live.matching(&filter);
    assert_eq!(editor(&state).live.entries(&matching).len(), 1);
    // Times change from run to run: fix them for the snapshot.
    let entries = editor(&state).live.entries(&(0..5).collect::<Vec<_>>());
    {
        let mut log = editor(&state).live.log();
        *log = RealtimeLog::new(10_000);
        for (n, e) in entries.into_iter().enumerate() {
            log.push(TimelineEntry {
                t_ms: 10 * n as u64,
                ..e
            });
        }
    }
    let screen = text(&state);
    assert!(screen.contains("Connected"), "{screen}");
    assert!(
        screen.contains("dir: received  type: all  text: trade"),
        "{screen}"
    );
    assert!(screen.contains("hello trade"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
    insta::assert_snapshot!("realtime_live_filtered", screen);

    state.shell.focus = Focus::Response;
    let effects = update(&mut state, Action::Key(event("Esc")));
    assert!(effects.is_empty());
    let done = running.await.unwrap();
    update(&mut state, done);
    let texts = server.sessions()[0].texts();
    assert_eq!(texts, [format!("token {CANARY}"), "hello trade".to_owned()]);
    let tab = state.tabs.active().unwrap();
    assert!(tab.in_flight.is_none());
    let record = &tab.exchange.as_ref().unwrap().record;
    let realtime = record.realtime.as_ref().unwrap();
    let end = realtime.end.as_ref().unwrap();
    assert_eq!(
        (end.reason.as_str(), end.close_code),
        ("cancelled", Some(1000))
    );
    let recorded = serde_json::to_string(realtime).unwrap();
    assert!(!recorded.contains(CANARY), "{recorded}");
    let screen = text(&state);
    assert!(screen.contains("Not connected"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
}

#[test]
fn filtering_twenty_sent_and_eighty_received_shows_received_trades_only() {
    let tmp = workspace("ws://127.0.0.1:9/feed");
    let mut state = shell(&tmp, 180, 30);
    open(&mut state, "feeds/echo.rman.yaml");
    {
        let mut log = editor(&state).live.log();
        for n in 0..20u64 {
            log.push(entry(Direction::Sent, n, "json", r#"{"op":"trade"}"#));
        }
        for n in 0..80u64 {
            let kind = if n % 2 == 0 { "trade" } else { "quote" };
            let payload = format!("{{\"n\":{n},\"kind\":\"{kind}\"}}");
            log.push(entry(Direction::Received, 20 + n, "json", &payload));
        }
    }
    state.shell.focus = Focus::EditorBody;
    editor_mut(&mut state).tab = RtTab::Timeline;
    press(&mut state, "d d");
    press(&mut state, "/");
    type_text(&mut state, "trade");
    press(&mut state, "Enter");
    let filter = editor(&state).filter.clone();
    assert_eq!(filter.dir, Some(Direction::Received));
    let matching = editor(&state).live.matching(&filter);
    let shown = editor(&state).live.entries(&matching);
    assert_eq!(shown.len(), 40);
    assert!(
        shown
            .iter()
            .all(|e| e.dir == Direction::Received && e.payload.contains("trade"))
    );
    let screen = text(&state);
    assert!(screen.contains("40 of 100 entries"), "{screen}");
}

#[test]
fn ten_thousand_entries_render_without_blocking_input() {
    let tmp = workspace("ws://127.0.0.1:9/feed");
    let mut state = shell(&tmp, 180, 30);
    open(&mut state, "feeds/echo.rman.yaml");
    {
        let mut log = editor(&state).live.log();
        for n in 0..10_000u64 {
            let dir = if n % 4 == 0 {
                Direction::Sent
            } else {
                Direction::Received
            };
            let payload = format!("{{\"n\":{n},\"kind\":\"trade\"}}");
            log.push(entry(dir, n, "json", &payload));
        }
        assert_eq!(log.len(), 10_000);
    }
    state.shell.focus = Focus::EditorBody;
    let started = Instant::now();
    let screen = text(&state);
    assert!(screen.contains("10000 of 10000 entries"), "{screen}");
    assert!(
        screen.contains("{\"n\":9999"),
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
    press(&mut state, "d d");
    let screen = text(&state);
    assert!(screen.contains("7500 of 10000 entries"), "{screen}");
    // A loose bound: the keys above are what show input is not blocked.
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(60),
        "23 keys and frames over 10000 entries took {elapsed:?}"
    );
    // Only the rows that fit are drawn.
    let rows = screen
        .lines()
        .filter(|l| l.contains("\"kind\":\"trade\""))
        .count();
    assert!(rows < 30, "{rows} rows drawn");
}
