//! The mock server pane (task 6.5), driven through `update` with keys;
//! jobs run synchronously the way the effect runner runs them, and the
//! live test runs the real server thread.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustman_core::Workspace;
use rustman_mock::{LogEntry, MockConfig, Source};
use tokio_util::sync::CancellationToken;

use super::{Job, LogRow, Msg, Prepared, Running, Server, StartJob};
use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::effects::execute;
use crate::snapshot::{buffer_text, render};
use crate::workbench_tests::event;

const API: &str = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /pets/{id}:\n    get:\n      operationId: getPet\n      parameters:\n        - {name: id, in: path, required: true, schema: {type: integer}}\n      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              examples:\n                cat: {value: {id: 1, name: Tom}}\n";

fn scratch() -> State {
    let mut state = State::with_screen(Screen::Shell(Session::Scratch));
    update(
        &mut state,
        Action::Resize {
            width: 100,
            height: 20,
        },
    );
    state
}

/// A shell over a workspace at `<tmp>/ws` with `settings` and `api.yaml`.
fn workspace(settings: &str) -> (tempfile::TempDir, State) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let toml = format!("[workspace]\nname = \"m\"\nversion = 1\n{settings}");
    std::fs::write(root.join("rustman.toml"), toml).unwrap();
    std::fs::write(root.join("api.yaml"), API).unwrap();
    let ws = Workspace::load(&root).unwrap();
    let mut state = scratch();
    state.screen = Screen::Shell(Session::Workspace(Box::new(ws)));
    (tmp, state)
}

fn keys(state: &mut State, keys: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for key in keys.split(' ') {
        effects = update(state, Action::Key(event(key)));
    }
    effects
}

fn msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    update(state, Action::Mock(Box::new(msg)))
}

fn screen(state: &mut State) -> String {
    while state.notifications.dismiss_newest() {}
    buffer_text(&render(state, 100, 20))
}

fn entry(method: &str, path: &str, status: u16, source: Source, matched: &str) -> LogEntry {
    LogEntry {
        time: "2026-11-18T10:15:30.125Z".into(),
        method: method.into(),
        path: path.into(),
        operation: Some(matched.into()),
        override_name: None,
        status: Some(status),
        source,
        errors: Vec::new(),
        duration_ms: 1.5,
        headers: Vec::new(),
        body: None,
    }
}

fn running(addr: &str) -> Box<Running> {
    Box::new(Running {
        addr: addr.parse().unwrap(),
        operations: 1,
        stop: CancellationToken::new(),
    })
}

#[test]
fn alt_o_opens_the_stopped_pane_and_esc_closes_it() {
    let mut state = scratch();
    keys(&mut state, "Alt+O");
    assert!(state.mock.open);
    insta::assert_snapshot!(screen(&mut state));
    keys(&mut state, "Esc");
    assert!(!state.mock.open);
    assert!(!screen(&mut state).contains("Mock server"));
}

#[test]
fn the_live_log_shows_status_and_source() {
    let mut state = scratch();
    keys(&mut state, "Alt+O");
    state.mock.document = "api.yaml".into();
    msg(&mut state, Msg::Started(Ok(running("127.0.0.1:4010"))));
    let example = entry("GET", "/pets/1", 200, Source::Example, "getPet");
    let mut invalid = entry("POST", "/pets", 422, Source::Error, "createPet");
    invalid.errors = vec!["body /name: 5 is not of type \"string\"".into()];
    let mut health = entry("GET", "/health", 200, Source::Override, "");
    health.operation = None;
    health.override_name = Some("health".into());
    for e in [example, invalid, health] {
        msg(&mut state, Msg::Entry(Box::new(e)));
    }
    msg(
        &mut state,
        Msg::Notice("mock: reloaded: 2 operations, 1 overrides".into()),
    );
    insta::assert_snapshot!(screen(&mut state));
    assert_eq!(state.mock.log.len(), 5);
}

#[test]
fn s_asks_for_the_document_then_prepares_and_starts() {
    let (_tmp, mut state) = workspace("[mock]\nport = 0\n");
    keys(&mut state, "Alt+O");
    keys(&mut state, "s");
    assert!(matches!(state.overlay, Some(Overlay::Input(_))));
    let effects = keys(&mut state, "a p i . y a m l Enter");
    let [Effect::Mock(job)] = effects.as_slice() else {
        panic!("expected a mock job, got {effects:?}");
    };
    assert!(matches!(**job, Job::Prepare { .. }));
    let done = execute(effects.into_iter().next().unwrap());
    let effects = update(&mut state, done);
    let [Effect::Mock(job)] = effects.as_slice() else {
        panic!("expected the start job, got {effects:?}");
    };
    let Job::Start(start) = &**job else {
        panic!("expected the start job");
    };
    assert_eq!(start.prepared.config.port, 0);
    assert!(start.prepared.document.ends_with("api.yaml"));
    assert!(matches!(state.mock.server, Server::Starting));
}

#[test]
fn a_network_address_needs_confirmation() {
    let mut state = scratch();
    keys(&mut state, "Alt+O");
    let prepared = Prepared {
        config: MockConfig {
            host: "0.0.0.0".into(),
            ..MockConfig::default()
        },
        document: "api.yaml".into(),
        mocks_dir: None,
    };
    msg(&mut state, Msg::Prepared(Ok(Arc::new(prepared))));
    let Some(Overlay::Confirm(confirm)) = &state.overlay else {
        panic!("expected a confirmation");
    };
    assert!(confirm.message.contains("reachable from the network"));
    assert!(matches!(state.mock.server, Server::Confirming(_)));
    // Cancel is preselected.
    keys(&mut state, "Enter");
    assert!(matches!(state.mock.server, Server::Stopped));
}

fn root(state: &State) -> PathBuf {
    crate::workbench::workspace(state).unwrap().root.clone()
}

#[test]
fn a_live_server_reports_each_request() {
    let (_tmp, state) = workspace("[mock]\nport = 0\n");
    let prepared = Prepared {
        config: MockConfig {
            port: 0,
            ..MockConfig::default()
        },
        document: root(&state).join("api.yaml").to_string_lossy().into_owned(),
        mocks_dir: Some(root(&state).join("mocks")),
    };
    let stop = CancellationToken::new();
    let job = StartJob {
        prepared: Arc::new(prepared),
        stop: stop.clone(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    super::spawn_server(job, tx);
    let next = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<Action>| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Action::Mock(msg)) = rx.try_recv() {
                return *msg;
            }
            assert!(Instant::now() < deadline, "no message from the server");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let Msg::Started(Ok(started)) = next(&mut rx) else {
        panic!("the server did not start");
    };
    let mut stream = std::net::TcpStream::connect(started.addr).unwrap();
    stream
        .write_all(b"GET /pets/1 HTTP/1.1\r\nHost: m\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let Msg::Entry(entry) = next(&mut rx) else {
        panic!("expected a log entry");
    };
    assert_eq!((entry.status, entry.source), (Some(200), Source::Example));
    assert_eq!(entry.operation.as_deref(), Some("getPet"));
    stop.cancel();
    assert!(matches!(next(&mut rx), Msg::Stopped));
    let mut state = state;
    msg(&mut state, Msg::Entry(entry));
    assert!(matches!(state.mock.log.back(), Some(LogRow::Request(_))));
}
