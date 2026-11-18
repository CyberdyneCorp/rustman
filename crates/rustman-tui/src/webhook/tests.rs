//! The webhook pane (spec: TUI webhook pane), driven through `update` with
//! keys; jobs run synchronously the way the effect runner runs them, and
//! the live-capture test runs the real listener thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::Version;
use rustman_core::{Redactor, Secret, Workspace};
use rustman_webhook::{Capture, CaptureStore, Event, HeaderEntry, ListenerConfig, Signature};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::{BodyMode, EventNote, Job, Listener, Msg, Prepared, Running};
use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::effects::execute;
use crate::snapshot::{buffer_text, render};
use crate::textarea::TextArea;
use crate::workbench_tests::event;

const CANARY: &str = "canary-5e1d07";

fn scratch() -> State {
    let mut state = State::with_screen(Screen::Shell(Session::Scratch));
    update(
        &mut state,
        Action::Resize {
            width: 110,
            height: 32,
        },
    );
    state
}

/// A shell over a fresh workspace at `<tmp>/ws`.
fn workspace() -> (tempfile::TempDir, State) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"hooks\"\nversion = 1\n",
    )
    .unwrap();
    let ws = Workspace::load(&root).unwrap();
    let mut state = scratch();
    state.screen = Screen::Shell(Session::Workspace(Box::new(ws)));
    (tmp, state)
}

fn root(state: &State) -> PathBuf {
    crate::workbench::workspace(state).unwrap().root.clone()
}

/// Press space-separated keys; the effects of the last one.
fn keys(state: &mut State, keys: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for key in keys.split(' ') {
        effects = update(state, Action::Key(event(key)));
    }
    effects
}

fn msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    update(state, Action::Webhook(Box::new(msg)))
}

/// Run effects and the effects their completions return.
fn run(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = execute(effect);
        effects.extend(update(state, done));
    }
}

fn redactor_with_canary() -> Redactor {
    let mut redactor = Redactor::new();
    redactor.add(&Secret::new("gh_hook", CANARY.to_owned()));
    redactor
}

/// Show a running listener on `addr`; its store.
fn started(state: &mut State, addr: &str, redactor: Redactor) -> CaptureStore {
    let store = CaptureStore::default();
    let running = Running {
        addr: addr.parse().unwrap(),
        store: store.clone(),
        stop: CancellationToken::new(),
        redactor,
    };
    msg(state, Msg::Started(Ok(Box::new(running))));
    store
}

pub(super) fn capture(
    method: &str,
    path: &str,
    body: &[u8],
    signature: Option<Signature>,
) -> Capture {
    Capture {
        id: 0,
        // 2025-11-18T10:15:30Z
        received_at: OffsetDateTime::from_unix_timestamp(1_763_460_930).unwrap(),
        remote: "127.0.0.1:53114".parse().unwrap(),
        version: Version::HTTP_11,
        method: method.into(),
        path: path.into(),
        query: None,
        headers: vec![
            HeaderEntry::new("Host", "127.0.0.1:9090"),
            HeaderEntry::new("Content-Type", "application/json"),
            HeaderEntry::new("Content-Length", body.len().to_string()),
        ],
        body: Bytes::copy_from_slice(body),
        status: 200,
        mock: None,
        signature,
    }
}

/// Store `capture` and deliver its event, as the listener thread does.
fn arrive(state: &mut State, store: &CaptureStore, capture: Capture) -> Arc<Capture> {
    let stored = store.push(capture);
    let event = Event::Captured(Arc::clone(&stored));
    msg(state, Msg::Event(EventNote::from(&event)));
    stored
}

/// The screen without toasts (they are covered by their own tests).
fn screen(state: &mut State) -> String {
    while state.notifications.dismiss_newest() {}
    buffer_text(&render(state, 110, 32))
}

fn effect_job(effects: &[Effect]) -> &Job {
    match effects {
        [Effect::Webhook(job)] => job,
        other => panic!("expected one webhook job, got {other:?}"),
    }
}

fn last_toast(state: &State) -> String {
    state.notifications.log().next().unwrap().message.clone()
}

#[test]
fn alt_w_opens_the_stopped_pane_and_esc_closes_it() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    assert!(state.webhook.open);
    insta::assert_snapshot!(screen(&mut state));
    keys(&mut state, "Esc");
    assert!(!state.webhook.open);
    assert!(!screen(&mut state).contains("Webhooks"));
}

#[test]
fn captures_list_newest_first_with_badges_and_the_detail_views() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", Redactor::new());
    arrive(
        &mut state,
        &store,
        capture("POST", "/hooks/stripe", b"{}", Some(Signature::Expired)),
    );
    arrive(
        &mut state,
        &store,
        capture("PUT", "/hooks/shop", b"{}", Some(Signature::Invalid)),
    );
    arrive(
        &mut state,
        &store,
        capture(
            "POST",
            "/hooks/github",
            br#"{"zen":"ok","n":1}"#,
            Some(Signature::Valid),
        ),
    );
    insta::assert_snapshot!(screen(&mut state));
    let ids: Vec<_> = state.webhook.captures.iter().map(|c| c.id).collect();
    assert_eq!(ids, [3, 2, 1], "newest first");

    // Text, JSON and hex views of the selected body.
    keys(&mut state, "v");
    assert_eq!(state.webhook.body, BodyMode::Text);
    assert!(screen(&mut state).contains(r#"{"zen":"ok","n":1}"#));
    keys(&mut state, "v v");
    assert_eq!(state.webhook.body, BodyMode::Hex);
    assert!(
        screen(&mut state).contains("00000000"),
        "{}",
        screen(&mut state)
    );

    // The selection follows its capture when a new one arrives on top.
    keys(&mut state, "Down");
    arrive(&mut state, &store, capture("GET", "/ping", b"", None));
    assert_eq!(super::selected(&state.webhook).unwrap().id, 2);
}

#[test]
fn a_binary_body_is_shown_as_hex() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", Redactor::new());
    arrive(
        &mut state,
        &store,
        capture("POST", "/bin", &[0xff, 0xfe, 0x00], None),
    );
    let text = screen(&mut state);
    assert!(text.contains("00000000  ff fe 00"), "{text}");
    keys(&mut state, "v");
    assert!(
        screen(&mut state).contains("00000000  ff fe 00"),
        "never decoded"
    );
}

#[test]
fn clear_empties_the_list_and_the_buffer() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", Redactor::new());
    arrive(&mut state, &store, capture("POST", "/a", b"x", None));
    keys(&mut state, "c");
    assert!(state.webhook.captures.is_empty());
    assert!(store.is_empty());
}

fn prepared(host: &str) -> Prepared {
    Prepared {
        config: ListenerConfig {
            host: host.into(),
            port: 0,
            ..ListenerConfig::default()
        },
        mocks: rustman_webhook::Mocks::empty(),
        verifiers: rustman_webhook::Verifiers::default(),
        root: None,
    }
}

#[test]
fn a_network_address_needs_confirmation_and_stays_flagged() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let effects = keys(&mut state, "s");
    assert!(matches!(
        effect_job(&effects),
        Job::Prepare { root: None, .. }
    ));
    let effects = msg(&mut state, Msg::Prepared(Ok(Arc::new(prepared("0.0.0.0")))));
    assert!(effects.is_empty(), "nothing starts before the user agrees");
    let Some(Overlay::Confirm(confirm)) = &state.overlay else {
        panic!("expected a confirmation");
    };
    assert!(
        confirm
            .message
            .contains("the webhook listener on 0.0.0.0:0 is reachable from the network"),
        "{}",
        confirm.message
    );
    // Cancel is preselected.
    keys(&mut state, "Enter");
    assert!(matches!(state.webhook.listener, Listener::Stopped));

    keys(&mut state, "s");
    msg(&mut state, Msg::Prepared(Ok(Arc::new(prepared("0.0.0.0")))));
    let effects = keys(&mut state, "s");
    let Job::Start(start) = effect_job(&effects) else {
        panic!("expected the listener to start");
    };
    assert!(start.prepared.config.allow_remote);

    started(&mut state, "0.0.0.0:9090", Redactor::new());
    assert!(screen(&mut state).contains("REACHABLE FROM THE NETWORK"));
    // The indicator stays in the status bar with the pane closed.
    keys(&mut state, "Esc");
    let text = screen(&mut state);
    assert!(text.contains("WEBHOOK ON NETWORK 0.0.0.0:9090"), "{text}");

    // A loopback address starts without asking.
    let mut state = scratch();
    keys(&mut state, "Alt+W s");
    let effects = msg(
        &mut state,
        Msg::Prepared(Ok(Arc::new(prepared("127.0.0.1")))),
    );
    assert!(matches!(effect_job(&effects), Job::Start(_)));
    assert!(state.overlay.is_none());
}

#[test]
fn a_bind_failure_is_shown() {
    let mut state = scratch();
    keys(&mut state, "Alt+W s");
    msg(
        &mut state,
        Msg::Started(Err(
            "webhook: cannot bind 127.0.0.1:9090: address in use".into()
        )),
    );
    assert!(matches!(state.webhook.listener, Listener::Failed(_)));
    let text = screen(&mut state);
    assert!(
        text.contains("Not running: webhook: cannot bind 127.0.0.1:9090: address in use"),
        "{text}"
    );
}

/// Apply actions from `rx` until `done` holds (at most 20 s).
async fn until(
    state: &mut State,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Action>,
    runner: &tokio::sync::mpsc::UnboundedSender<Effect>,
    done: impl Fn(&State) -> bool,
) {
    let wait = async {
        while !done(state) {
            let action = rx.recv().await.expect("the runner is alive");
            for effect in update(state, action) {
                runner.send(effect).unwrap();
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait)
        .await
        .expect("condition not reached in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capture_appears_at_the_top_within_200_ms() {
    // Scenario "Live capture", through the real listener thread.
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = crate::effects::spawn_runner(tx);
    state.webhook.listener = Listener::Preparing;
    for effect in msg(
        &mut state,
        Msg::Prepared(Ok(Arc::new(prepared("127.0.0.1")))),
    ) {
        runner.send(effect).unwrap();
    }
    until(&mut state, &mut rx, &runner, |s| {
        matches!(s.webhook.listener, Listener::Running(_))
    })
    .await;
    let Listener::Running(running) = &state.webhook.listener else {
        unreachable!()
    };
    let addr = running.addr;

    let mut samples = Vec::new();
    for n in 1..=5 {
        let path = format!("/hooks/live-{n}");
        let request = format!("POST {path} HTTP/1.1\r\nHost: h\r\nContent-Length: 2\r\n\r\n{{}}");
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
        })
        .await
        .unwrap();
        // The capture is stored before the reply is written.
        let replied = Instant::now();
        until(&mut state, &mut rx, &runner, |s| {
            s.webhook.captures.first().is_some_and(|c| c.path == path)
        })
        .await;
        samples.push(replied.elapsed());
        let text = screen(&mut state);
        let row = text.lines().find(|l| l.contains("/hooks/live")).unwrap();
        assert!(row.contains(&path), "the newest is the top row:\n{text}");
    }
    crate::testing::assert_timing(
        "capture to pane",
        &samples,
        Duration::from_millis(200),
        Duration::from_millis(600),
    );

    // `s` stops the listener and releases the port.
    keys(&mut state, "s");
    until(&mut state, &mut rx, &runner, |s| {
        matches!(s.webhook.listener, Listener::Stopped)
    })
    .await;
    assert!(state.status.webhook.is_none());
    assert!(
        port_released(addr, Duration::from_secs(10)),
        "nothing may listen on {addr} any more"
    );
}

/// Whether nothing of ours listens on `addr`. Under a parallel test run
/// another socket may briefly take the same port number: a client socket
/// keeps it from being bound but refuses connections, another test's server
/// accepts them but goes away. Either check passing within a generous
/// window means the listener let go of the port.
fn port_released(addr: std::net::SocketAddr, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        if std::net::TcpListener::bind(addr).is_ok() {
            return true;
        }
        let refused = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
        if refused {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_port_still_listened_on_is_not_released() {
    // Guard for the check above: it must not pass while a listener lives.
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = held.local_addr().unwrap();
    assert!(!port_released(addr, Duration::from_millis(300)));
}

#[test]
fn the_mock_editor_saves_valid_mocks_and_keeps_errors_in_the_editor() {
    let (_tmp, mut state) = workspace();
    let file = root(&state).join("webhook/mocks.yaml");
    keys(&mut state, "Alt+W");
    let effects = keys(&mut state, "m");
    run(&mut state, effects);
    let editor = state.webhook.mocks.as_ref().expect("the editor opened");
    assert!(editor.text.text().contains("mocks: []"));
    assert!(screen(&mut state).contains("Mocks: webhook/mocks.yaml"));

    let set = |state: &mut State, text: &str| {
        state.webhook.mocks.as_mut().unwrap().text = TextArea::from_text(text);
    };
    set(&mut state, "mocks:\n- {path: /a, delay_ms: 90000}\n");
    let effects = keys(&mut state, "Ctrl+S");
    run(&mut state, effects);
    let error = state.webhook.mocks.as_ref().unwrap().error.clone().unwrap();
    assert!(error.contains("delay_ms 90000 exceeds 60000"), "{error}");
    assert!(!file.exists(), "an invalid file is never written");
    assert!(screen(&mut state).contains("exceeds 60000"));

    set(
        &mut state,
        "mocks:\n- {method: POST, path: /hooks/*, status: 500}\n",
    );
    let effects = keys(&mut state, "Ctrl+S");
    run(&mut state, effects);
    assert!(state.webhook.mocks.is_none(), "saved and closed");
    assert_eq!(last_toast(&state), "Mocks saved: 1 defined");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("status: 500"));

    // Typing edits; Esc closes without saving.
    let effects = keys(&mut state, "m");
    run(&mut state, effects);
    keys(&mut state, "x");
    assert!(
        state
            .webhook
            .mocks
            .as_ref()
            .unwrap()
            .text
            .text()
            .starts_with('x')
    );
    keys(&mut state, "Esc");
    assert!(state.webhook.mocks.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), saved);
}

#[test]
fn mocks_need_a_workspace() {
    let mut state = scratch();
    let effects = keys(&mut state, "Alt+W m");
    assert!(effects.is_empty());
    assert!(last_toast(&state).contains("webhook/mocks.yaml"));
}

/// The test server on its own runtime.
fn echo_server() -> (tokio::runtime::Runtime, rustman_test_support::TestServer) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(rustman_test_support::TestServer::http());
    (runtime, server)
}

fn set_input(state: &mut State, text: &str) {
    let Some(Overlay::Input(input)) = &mut state.overlay else {
        panic!("expected a prompt, got {:?}", state.overlay);
    };
    input.text = TextArea::from_text(text);
}

#[test]
fn replay_save_history_and_copy_as_curl() {
    let (_tmp, mut state) = workspace();
    let root = root(&state);
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", Redactor::new());
    arrive(
        &mut state,
        &store,
        capture("POST", "/hooks/github", br#"{"a":1}"#, None),
    );

    // Replay to a local server.
    let (_runtime, server) = echo_server();
    keys(&mut state, "r");
    let Some(Overlay::Input(input)) = &state.overlay else {
        panic!("expected the URL prompt");
    };
    assert_eq!(input.text.text(), "http://localhost:3000/hooks/github");
    set_input(&mut state, &server.url("/echo"));
    let effects = keys(&mut state, "Enter");
    run(&mut state, effects);
    assert_eq!(
        last_toast(&state),
        format!("Replayed #1 to {}: 200", server.url("/echo"))
    );

    // Save as a request file.
    keys(&mut state, "w");
    let Some(Overlay::Input(input)) = &state.overlay else {
        panic!("expected the URL prompt");
    };
    assert_eq!(input.text.text(), "{{base_url}}/hooks/github");
    keys(&mut state, "Enter");
    let effects = keys(&mut state, "Enter");
    run(&mut state, effects);
    let saved = std::fs::read_to_string(root.join("webhook-1.rman.yaml")).unwrap();
    assert!(saved.contains("kind: http"), "{saved}");
    assert!(
        saved.contains("url: '{{base_url}}/hooks/github'"),
        "{saved}"
    );
    assert!(!saved.contains("Content-Length"), "{saved}");
    assert!(
        last_toast(&state).starts_with("Saved #1 as "),
        "{}",
        last_toast(&state)
    );

    // Save to history.
    let effects = keys(&mut state, "h");
    run(&mut state, effects);
    assert!(last_toast(&state).starts_with("Saved #1 to history as "));
    let entries = std::fs::read_dir(root.join(".rustman/history"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json")
        })
        .count();
    assert_eq!(entries, 1);

    // Copy as curl, to the URL the capture was sent to.
    let effects = keys(&mut state, "y");
    let [Effect::Copy(command)] = effects.as_slice() else {
        panic!("expected a copy, got {effects:?}");
    };
    assert!(
        command.starts_with("curl 'http://127.0.0.1:9090/hooks/github'"),
        "{command}"
    );
    assert!(!command.contains("Content-Length"), "{command}");
}

#[test]
fn verifier_secrets_never_reach_the_pane_the_log_or_curl() {
    let (_tmp, mut state) = workspace();
    let root = root(&state);
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", redactor_with_canary());
    let mut leaky = capture(
        "POST",
        &format!("/hooks/{CANARY}"),
        format!(r#"{{"echo":"{CANARY}"}}"#).as_bytes(),
        Some(Signature::Valid),
    );
    leaky.headers.push(HeaderEntry::new("X-Echo", CANARY));
    arrive(&mut state, &store, leaky);

    for mode in ["", "v", "v", "v"] {
        if !mode.is_empty() {
            keys(&mut state, mode);
        }
        let text = screen(&mut state);
        assert!(!text.contains(CANARY), "{text}");
        assert!(text.contains("«redacted:gh_hook»"), "{text}");
    }
    assert!(state.webhook.log.iter().all(|l| !l.contains(CANARY)));
    let effects = keys(&mut state, "y");
    let [Effect::Copy(command)] = effects.as_slice() else {
        panic!("expected a copy");
    };
    assert!(!command.contains(CANARY), "{command}");

    // Saving a capture that holds a secret is refused.
    keys(&mut state, "w Enter");
    let effects = keys(&mut state, "Enter");
    run(&mut state, effects);
    assert!(last_toast(&state).contains("holds a secret value"));
    assert!(!root.join("webhook-1.rman.yaml").exists());
    let effects = keys(&mut state, "h");
    run(&mut state, effects);
    for entry in std::fs::read_dir(root.join(".rustman/history")).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(CANARY), "{text}");
    }
}

#[test]
fn prepare_reads_the_bind_address_and_mocks_of_the_workspace() {
    let (_tmp, state) = workspace();
    let root = root(&state);
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"hooks\"\nversion = 1\n\n[webhook]\nhost = \"0.0.0.0\"\nport = 9191\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("webhook")).unwrap();
    std::fs::write(
        root.join("webhook/mocks.yaml"),
        "mocks:\n- {path: /a, status: 201}\n",
    )
    .unwrap();
    let Action::Webhook(msg) = execute(Effect::Webhook(Box::new(Job::Prepare {
        root: Some(root.clone()),
        environment: None,
        backend: None,
    }))) else {
        panic!("expected a webhook message");
    };
    let Msg::Prepared(Ok(prepared)) = *msg else {
        panic!("expected settings");
    };
    assert_eq!(
        (prepared.config.host.as_str(), prepared.config.port),
        ("0.0.0.0", 9191)
    );
    assert!(!prepared.config.allow_remote);
    assert_eq!(prepared.mocks.len(), 1);
    assert_eq!(prepared.root.as_deref(), Some(Path::new(&root)));
}

#[test]
fn a_long_body_scrolls_by_page_and_is_built_once() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", Redactor::new());
    let body: String = (0..400).map(|n| format!("line-{n:03}\n")).collect();
    let mut long = capture("POST", "/long", body.as_bytes(), None);
    long.headers[1] = HeaderEntry::new("Content-Type", "text/plain");
    arrive(&mut state, &store, long);

    let text = screen(&mut state);
    assert!(text.contains("line-000"), "{text}");
    assert!(!text.contains("line-399"), "{text}");
    for _ in 0..3 {
        screen(&mut state);
    }
    assert_eq!(
        state.webhook.body_cache.builds(),
        1,
        "redrawn from the cache"
    );

    keys(&mut state, "PageDown");
    let text = screen(&mut state);
    assert!(!text.contains("line-000"), "{text}");
    assert!(text.contains("line-"), "{text}");

    // Far past the end: the last page stays full, and one PageUp moves back.
    for _ in 0..100 {
        keys(&mut state, "PageDown");
    }
    let text = screen(&mut state);
    assert!(text.contains("line-399"), "{text}");
    assert!(text.contains("of 406"), "the position is shown: {text}");
    keys(&mut state, "PageUp");
    assert!(!screen(&mut state).contains("line-399"));

    // Another body mode starts at the top again and builds its lines once.
    keys(&mut state, "v");
    let text = screen(&mut state);
    assert!(text.contains("line-000"), "{text}");
    screen(&mut state);
    assert_eq!(state.webhook.body_cache.builds(), 2);
}

#[test]
fn captures_stay_redacted_after_the_listener_stopped() {
    let mut state = scratch();
    keys(&mut state, "Alt+W");
    let store = started(&mut state, "127.0.0.1:9090", redactor_with_canary());
    let body = format!(r#"{{"echo":"{CANARY}"}}"#);
    arrive(
        &mut state,
        &store,
        capture("POST", "/a", body.as_bytes(), None),
    );
    keys(&mut state, "s");
    msg(&mut state, Msg::Stopped);
    assert!(matches!(state.webhook.listener, Listener::Stopped));
    let text = screen(&mut state);
    assert!(text.contains("/a"), "{text}");
    assert!(!text.contains(CANARY), "{text}");
    assert!(text.contains("«redacted:gh_hook»"), "{text}");
    let effects = keys(&mut state, "y");
    let [Effect::Copy(command)] = effects.as_slice() else {
        panic!("expected a copy");
    };
    assert!(!command.contains(CANARY), "{command}");
}
