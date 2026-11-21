//! The recorder pane (traffic-recorder spec: Live capture list, Redaction
//! in display and export, HAR export), driven through `update` with keys;
//! jobs run synchronously the way the effect runner runs them, and the
//! live test runs the real proxy thread against a local upstream.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustman_core::Workspace;
use rustman_proxy::capture::{BodyCapture, Tunnel};
use rustman_proxy::{CaptureStore, Exchange, ProxyConfig, Recorded};
use tokio_util::sync::CancellationToken;

use super::{Job, Msg, Prepared, Proxy, Running};
use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::dialogs::Decision;
use crate::effects::execute;
use crate::snapshot::{buffer_text, render};
use crate::workbench_tests::event;

const TOKEN: &str = "tok-5e1d07abcdef";
const COOKIE: &str = "sid=c00kie-9a8b7c";

fn scratch() -> State {
    let mut state = State::with_screen(Screen::Shell(Session::Scratch));
    update(
        &mut state,
        Action::Resize {
            width: 120,
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
        "[workspace]\nname = \"rec\"\nversion = 1\n",
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
    update(state, Action::Recorder(Box::new(msg)))
}

/// Run effects and the effects their completions return.
fn run(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = execute(effect);
        effects.extend(update(state, done));
    }
}

/// Show a running proxy on `addr`; its store.
fn started(state: &mut State, addr: &str) -> CaptureStore {
    let store = CaptureStore::new(100, usize::MAX);
    let running = Running {
        addr: addr.parse().unwrap(),
        store: store.clone(),
        stop: CancellationToken::new(),
        intercept: vec!["*.example.com".into()],
    };
    msg(state, Msg::Started(Ok(Box::new(running))));
    store
}

fn body(bytes: &[u8]) -> BodyCapture {
    BodyCapture {
        bytes: bytes.to_vec(),
        total: bytes.len() as u64,
        truncated: false,
    }
}

fn exchange(method: &str, url: &str) -> Recorded {
    Recorded::Exchange(Box::new(Exchange {
        method: method.into(),
        url: url.into(),
        version: "HTTP/1.1".into(),
        request_headers: vec![
            ("authorization".into(), format!("Bearer {TOKEN}")),
            ("cookie".into(), COOKIE.into()),
            ("accept".into(), "application/json".into()),
        ],
        request_body: BodyCapture::default(),
        status: Some(200),
        response_headers: vec![("content-type".into(), "application/json".into())],
        response_body: body(format!(r#"{{"echo":"{TOKEN}","n":1}}"#).as_bytes()),
        duration: Duration::from_millis(12),
        intercepted: true,
        error: None,
    }))
}

fn tunnel() -> Recorded {
    Recorded::Tunnel(Tunnel {
        host: "www.google.com".into(),
        port: 443,
        bytes_up: 517,
        bytes_down: 4096,
        duration: Duration::from_millis(30),
        error: None,
    })
}

/// 10:15:30 UTC.
fn at() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_763_460_930)
}

/// Store `recorded` and announce it, as the proxy thread does.
fn arrive(state: &mut State, store: &CaptureStore, recorded: Recorded) {
    store.push(at(), recorded);
    msg(state, Msg::Captured);
}

/// Three captures: an exchange, a tunnel and a rejected handshake.
fn recording(state: &mut State) -> CaptureStore {
    keys(state, "Alt+P");
    let store = started(state, "127.0.0.1:8888");
    arrive(
        state,
        &store,
        exchange("GET", "https://api.example.com/v1/me"),
    );
    arrive(state, &store, tunnel());
    let rejected = Recorded::TlsRejected {
        host: "pinned.example.com".into(),
        port: 443,
    };
    arrive(state, &store, rejected);
    store
}

/// The screen without toasts.
fn screen(state: &mut State) -> String {
    while state.notifications.dismiss_newest() {}
    buffer_text(&render(state, 120, 32))
}

fn effect_job(effects: &[Effect]) -> &Job {
    match effects {
        [Effect::Recorder(job)] => job,
        other => panic!("expected one recorder job, got {other:?}"),
    }
}

fn last_toast(state: &State) -> String {
    state.notifications.log().next().unwrap().message.clone()
}

#[test]
fn alt_p_opens_the_stopped_pane_and_esc_closes_it() {
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    assert!(state.recorder.open);
    let text = screen(&mut state);
    assert!(
        text.contains("Press s to start the recording proxy"),
        "{text}"
    );
    keys(&mut state, "Esc");
    assert!(!state.recorder.open);
    assert!(!screen(&mut state).contains("Traffic recorder"));
}

#[test]
fn recorder_list_shows_captures_newest_first() {
    let mut state = scratch();
    recording(&mut state);
    let ids: Vec<u64> = state.recorder.captures.iter().map(|c| c.id).collect();
    assert_eq!(ids, [3, 2, 1]);
    keys(&mut state, "j Space");
    let text = screen(&mut state);
    insta::assert_snapshot!(text);
    assert!(
        text.contains("* 10:15:30 TUNNEL www.google.com:443"),
        "{text}"
    );
}

#[test]
fn recorder_detail_is_redacted() {
    // Scenario "Cookie hidden in the pane".
    let mut state = scratch();
    recording(&mut state);
    keys(&mut state, "G");
    let text = screen(&mut state);
    insta::assert_snapshot!(text);
    assert!(text.contains("cookie: «redacted:Cookie»"), "{text}");
    for secret in [TOKEN, COOKIE] {
        assert!(!text.contains(secret), "{secret} shown:\n{text}");
    }
}

#[test]
fn recorder_detail_redacts_secret_named_headers_query_and_body_fields() {
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    let store = started(&mut state, "127.0.0.1:8888");
    let mut recorded = exchange(
        "POST",
        "https://api.example.com/login?api_key=QWERTY987654321secret",
    );
    if let Recorded::Exchange(e) = &mut recorded {
        e.request_headers
            .push(("x-api-key".into(), "sk_live_ABCDEF1234567890".into()));
        e.request_body = body(br#"{"user":"me","password":"hunter2hunter2"}"#);
    }
    arrive(&mut state, &store, recorded);
    let text = screen(&mut state);
    assert!(text.contains("x-api-key"), "{text}");
    for secret in [
        "QWERTY987654321secret",
        "sk_live_ABCDEF1234567890",
        "hunter2hunter2",
    ] {
        assert!(!text.contains(secret), "{secret} shown:\n{text}");
    }
}

#[test]
fn recorder_status_badges() {
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    started(&mut state, "127.0.0.1:8888");
    msg(&mut state, Msg::CaTrusted(true));
    keys(&mut state, "Esc");
    let text = screen(&mut state);
    let status = text.lines().last().unwrap().to_owned();
    insta::assert_snapshot!(status);
    assert!(status.contains(" CA TRUSTED "), "{status}");
    assert!(status.contains("record 127.0.0.1:8888"), "{status}");
    let buffer = render(&state, 120, 32);
    let style = crate::snapshot::style_of(&buffer, "CA TRUSTED").unwrap();
    assert_eq!(style.bg, state.ui.theme.color(crate::theme::Token::Warn));
}

#[test]
fn a_network_reachable_proxy_is_a_red_badge() {
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    started(&mut state, "0.0.0.0:8888");
    let text = screen(&mut state);
    assert!(
        text.contains(" RECORDER ON NETWORK 0.0.0.0:8888 "),
        "{text}"
    );
    assert!(text.contains("REACHABLE FROM THE NETWORK"), "{text}");
}

#[test]
fn the_filter_narrows_the_list_and_esc_clears_it() {
    let mut state = scratch();
    recording(&mut state);
    keys(&mut state, "/ t u n");
    assert!(state.recorder.filtering);
    let shown: Vec<u64> = super::visible(&state.recorder)
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(shown, [2]);
    assert!(screen(&mut state).contains("Filter: tun_"));
    keys(&mut state, "Enter");
    assert!(!state.recorder.filtering);
    assert!(screen(&mut state).contains("Filter: tun (1 shown)"));
    keys(&mut state, "/ Esc");
    assert_eq!(super::visible(&state.recorder).len(), 3);
}

#[test]
fn space_marks_and_a_marks_all() {
    let mut state = scratch();
    recording(&mut state);
    keys(&mut state, "Space j Space");
    assert_eq!(
        state.recorder.marked.iter().copied().collect::<Vec<_>>(),
        [2, 3]
    );
    keys(&mut state, "Space");
    assert_eq!(
        state.recorder.marked.iter().copied().collect::<Vec<_>>(),
        [3]
    );
    keys(&mut state, "a");
    assert_eq!(state.recorder.marked.len(), 3);
    keys(&mut state, "a");
    assert!(state.recorder.marked.is_empty());
}

#[test]
fn save_writes_request_files_without_secrets() {
    let (_tmp, mut state) = workspace();
    let store = recording(&mut state);
    arrive(
        &mut state,
        &store,
        exchange("GET", "https://api.example.com/users/17"),
    );
    keys(&mut state, "a");
    let effects = keys(&mut state, "w");
    assert!(matches!(effect_job(&effects), Job::Save { captures, .. } if captures.len() == 4));
    run(&mut state, effects);
    let toast = last_toast(&state);
    assert!(toast.starts_with("Saved 2 request files"), "{toast}");
    let root = root(&state);
    let file =
        std::fs::read_to_string(root.join("recorded/api-example-com/get-users-id.rman.yaml"))
            .unwrap();
    assert!(file.contains("{{base_url}}/users/{{user_id}}"), "{file}");
    assert!(file.contains("Bearer {{auth_token}}"), "{file}");
    for path in walk(&root) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!text.contains(TOKEN), "token in {}", path.display());
        assert!(
            !text.contains("c00kie-9a8b7c"),
            "cookie in {}",
            path.display()
        );
    }
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn saving_needs_a_workspace_and_a_capture() {
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    keys(&mut state, "w");
    assert_eq!(last_toast(&state), "No capture selected");
    let store = started(&mut state, "127.0.0.1:8888");
    arrive(&mut state, &store, tunnel());
    assert!(keys(&mut state, "w").is_empty());
    assert_eq!(last_toast(&state), "Saving captures needs a workspace");
}

#[test]
fn export_writes_a_redacted_har_of_the_marked_captures() {
    let (_tmp, mut state) = workspace();
    recording(&mut state);
    keys(&mut state, "j Space j Space");
    keys(&mut state, "e");
    let Some(Overlay::Input(input)) = &state.overlay else {
        panic!("the HAR prompt opens");
    };
    assert_eq!(input.text.text(), super::DEFAULT_HAR);
    let effects = keys(&mut state, "Enter");
    assert!(matches!(effect_job(&effects), Job::ExportHar { captures, .. } if captures.len() == 2));
    run(&mut state, effects);
    assert!(last_toast(&state).starts_with("Exported 2 captures to "));
    let text = std::fs::read_to_string(root(&state).join(super::DEFAULT_HAR)).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let entries = doc["log"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["request"]["method"], "GET", "oldest first");
    assert_eq!(entries[1]["request"]["method"], "CONNECT");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("«redacted:Authorization»"), "{text}");
}

#[test]
fn starting_reads_the_settings_and_a_remote_host_needs_confirmation() {
    let (_tmp, mut state) = workspace();
    let root = root(&state);
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"rec\"\nversion = 1\n[record]\nhost = \"0.0.0.0\"\nport = 0\n",
    )
    .unwrap();
    state.secret_backend = Some(rustman_core::SecretBackend::File);
    keys(&mut state, "Alt+P");
    let effects = keys(&mut state, "s");
    assert!(matches!(effect_job(&effects), Job::Prepare { .. }));
    run(&mut state, effects);
    assert!(matches!(state.recorder.proxy, Proxy::Confirming(_)));
    let Some(Overlay::Confirm(confirm)) = &state.overlay else {
        panic!("a confirmation opens");
    };
    assert!(
        confirm.message.contains("reachable from the network"),
        "{}",
        confirm.message
    );
    assert_eq!(confirm.choices[confirm.selected].1, Decision::Cancel);
    crate::workbench::decide(&mut state, Decision::Cancel);
    assert!(matches!(state.recorder.proxy, Proxy::Stopped));

    state.recorder.proxy = Proxy::Confirming(Arc::new(Prepared {
        config: ProxyConfig {
            host: "0.0.0.0".into(),
            ..ProxyConfig::default()
        },
        ca: None,
        ca_trusted: false,
    }));
    let effects = crate::workbench::decide(&mut state, Decision::RecorderStart);
    match effect_job(&effects) {
        Job::Start(start) => assert!(start.prepared.config.allow_remote),
        other => panic!("{other:?}"),
    }
}

#[test]
fn intercepting_without_a_ca_fails_with_the_init_hint() {
    let (_tmp, mut state) = workspace();
    let root = root(&state);
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"rec\"\nversion = 1\n[record]\nintercept = [\"localhost\"]\n",
    )
    .unwrap();
    state.secret_backend = Some(rustman_core::SecretBackend::File);
    keys(&mut state, "Alt+P");
    let effects = keys(&mut state, "s");
    run(&mut state, effects);
    let Proxy::Failed(message) = &state.recorder.proxy else {
        panic!("{:?}", state.recorder.proxy);
    };
    assert_eq!(
        message,
        "record: this workspace has no local CA; run 'rustman record ca init'"
    );
}

#[test]
fn job_debug_never_shows_capture_contents() {
    let store = CaptureStore::new(10, usize::MAX);
    let capture = store.push(at(), exchange("GET", "https://api.example.com/"));
    let job = Job::ExportHar {
        path: PathBuf::from("x.har"),
        captures: vec![capture],
    };
    let debug = format!("{job:?}");
    assert!(!debug.contains(TOKEN), "{debug}");
    assert!(debug.contains("captures: [1]"), "{debug}");
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
async fn a_proxied_request_appears_in_the_pane_redacted() {
    let upstream = rustman_test_support::server::TestServer::http().await;
    let mut state = scratch();
    keys(&mut state, "Alt+P");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = crate::effects::spawn_runner(tx);
    state.recorder.proxy = Proxy::Preparing;
    let prepared = Prepared {
        config: ProxyConfig {
            port: 0,
            ..ProxyConfig::default()
        },
        ca: None,
        ca_trusted: false,
    };
    for effect in msg(&mut state, Msg::Prepared(Ok(Arc::new(prepared)))) {
        runner.send(effect).unwrap();
    }
    until(&mut state, &mut rx, &runner, |s| {
        matches!(s.recorder.proxy, Proxy::Running(_))
    })
    .await;
    let Proxy::Running(running) = &state.recorder.proxy else {
        unreachable!()
    };
    let addr = running.addr;
    assert!(addr.ip().is_loopback());
    let url = upstream.url("/echo");
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut tcp = std::net::TcpStream::connect(addr).unwrap();
        let head = format!(
            "GET {url} HTTP/1.1\r\nHost: x\r\nCookie: {COOKIE}\r\nConnection: close\r\n\r\n"
        );
        tcp.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        tcp.read_to_end(&mut response).unwrap();
    })
    .await
    .unwrap();
    until(&mut state, &mut rx, &runner, |s| {
        !s.recorder.captures.is_empty()
    })
    .await;
    let text = screen(&mut state);
    assert!(text.contains("cookie: «redacted:Cookie»"), "{text}");
    assert!(!text.contains("c00kie-9a8b7c"), "{text}");

    keys(&mut state, "s");
    until(&mut state, &mut rx, &runner, |s| {
        matches!(s.recorder.proxy, Proxy::Stopped)
    })
    .await;
    assert!(state.status.recorder.is_none());
}
