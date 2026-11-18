//! Sending from the TUI end to end, against the in-process test server
//! (tasks 5.1, 5.2 and the response viewer's secret canary): the real loop,
//! effect runner and engine, with a recording terminal backend.

use std::convert::Infallible;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::TestServer;
use rustman_test_support::routes::timed_hits;
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{Action, Effect, State, update};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::response::{BodyKind, ResponseTab};
use crate::runtime::{Headless, run_app};
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;

/// A secret value that must never be drawn.
const CANARY: &str = "canary-9c1e-token";

/// A workspace whose requests call `server`, with the environment `local`
/// holding the secret `token` (set to [`CANARY`] in the file store).
fn workspace(server: &TestServer, slow_id: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: String| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n"
            .into(),
    );
    write(
        "environments/local.yaml",
        format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.url("")
        ),
    );
    write(
        "slow.rman.yaml",
        format!(
            "version: 1\nkind: http\nname: Slow\nmethod: GET\nurl: '{{{{base_url}}}}/timed/10000?id={slow_id}'\n"
        ),
    );
    write(
        "user.rman.yaml",
        "version: 1\nkind: http\nname: User\nmethod: GET\nurl: '{{base_url}}/users/42'\n\
         assertions:\n  - status: 200\n  - json:\n      path: $.id\n      equals: 42\n"
            .into(),
    );
    write(
        "echo.rman.yaml",
        "version: 1\nkind: http\nname: Echo\nmethod: GET\n\
         url: '{{base_url}}/echo?t={{token}}'\nheaders:\n\
         - name: Authorization\n  value: Bearer {{token}}\n\
         - name: X-Echo\n  value: '{{token}}'\n\
         captures:\n  - var: echoed\n    from: json\n    path: $.path\n  - var: method\n    from: json\n    path: $.method\n"
            .into(),
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    tmp
}

/// The workspace loaded into a shell of `width` x `height`, with the file
/// secret store, and `request` open.
fn shell(tmp: &tempfile::TempDir, request: &str, width: u16, height: u16) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, mut effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::NoColor,
    );
    while let Some(effect) = effects.pop() {
        effects.extend(update(&mut state, execute(effect)));
    }
    state.secret_backend = Some(SecretBackend::File);
    update(&mut state, Action::Resize { width, height });
    crate::workbench::open_path(&mut state, Path::new(request));
    state.shell.focus = Focus::EditorBody;
    state
}

/// Send the open request and apply the result, the way the runner would.
async fn send(state: &mut State) {
    let effects = crate::send::start(state);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("expected a send, got {effects:?}");
    };
    let job = job.clone();
    let done = tokio::task::spawn_blocking(move || crate::send::execute(&job))
        .await
        .unwrap();
    update(state, done);
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

/// A terminal backend that reports every drawn frame. A selected sidebar
/// row (drawn reversed without colour) is appended as `selected: <row>`.
pub(crate) struct Recording {
    pub(crate) inner: TestBackend,
    pub(crate) frames: mpsc::Sender<(Instant, String)>,
}

impl Backend for Recording {
    type Error = Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Infallible>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Infallible> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Infallible> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Infallible> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Infallible> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Infallible> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Infallible> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Infallible> {
        let mut frame = buffer_text(self.inner.buffer());
        if let Some(row) = selected_row(self.inner.buffer()) {
            frame.push_str(&format!("selected: {row}\n"));
        }
        let _ = self.frames.send((Instant::now(), frame));
        self.inner.flush()
    }
}

/// The text of the first row whose first sidebar columns are drawn
/// reversed.
fn selected_row(buffer: &ratatui::buffer::Buffer) -> Option<String> {
    let area = buffer.area;
    (area.top()..area.bottom())
        .find(|&y| {
            (1..4).any(|x| {
                buffer[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
        })
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
}

pub(crate) fn key(text: &str) -> Action {
    let chord = crate::keymap::chord::parse_chord(text).unwrap();
    let code = match chord.code {
        crate::keymap::chord::Code::Char(c) => KeyCode::Char(c),
        crate::keymap::chord::Code::Esc => KeyCode::Esc,
        crate::keymap::chord::Code::Down => KeyCode::Down,
        crate::keymap::chord::Code::Up => KeyCode::Up,
        crate::keymap::chord::Code::Enter => KeyCode::Enter,
        other => panic!("unsupported key {other:?}"),
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, chord.ctrl);
    Action::Key(KeyEvent::new(code, modifiers))
}

/// The first frame drawn after `after` that `matches`, within 10 s.
pub(crate) fn frame_after(
    frames: &mpsc::Receiver<(Instant, String)>,
    after: Instant,
    matches: impl Fn(&str) -> bool,
) -> (Instant, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let (at, frame) = frames
            .recv_timeout(left)
            .expect("no matching frame within 10 s");
        if at >= after && matches(&frame) {
            return (at, frame);
        }
    }
}

/// What the scripted user saw.
struct Seen {
    key_latencies: Vec<Duration>,
    elapsed_texts: Vec<String>,
    cancelled: String,
    idle_hint: String,
}

/// The selected sidebar row in a frame.
fn selection(frame: &str) -> Option<&str> {
    frame.lines().find_map(|l| l.strip_prefix("selected: "))
}

/// The user: send the slow request, move through the sidebar and toggle it
/// while the request is in flight, watch the counter, cancel, press Ctrl+C
/// idle, quit.
fn script(
    tx: tokio::sync::mpsc::UnboundedSender<Action>,
    frames: &mpsc::Receiver<(Instant, String)>,
) -> Seen {
    let sending = |f: &str| f.contains("Sending…");
    let start = Instant::now();
    tx.send(key("Ctrl+R")).unwrap();
    let (_, first) = frame_after(frames, start, sending);
    let mut key_latencies = Vec::new();
    let mut selected = selection(&first)
        .expect("the sidebar is focused")
        .to_owned();
    for chord in ["Down", "Down", "Up", "Down", "Up", "Up"] {
        let at = Instant::now();
        tx.send(key(chord)).unwrap();
        let (drawn, frame) = frame_after(frames, at, |f| {
            selection(f).is_some_and(|row| row != selected)
        });
        selected = selection(&frame).unwrap().to_owned();
        key_latencies.push(drawn - at);
    }
    for i in 0..6 {
        let hidden = i % 2 == 0;
        let at = Instant::now();
        tx.send(key("Ctrl+B")).unwrap();
        let (drawn, _) = frame_after(frames, at, |f| f.contains(" Sidebar ") != hidden);
        key_latencies.push(drawn - at);
    }
    std::thread::sleep(Duration::from_millis(400));
    let (_, later) = frame_after(frames, Instant::now(), sending);
    let elapsed_texts = [first, later]
        .iter()
        .map(|f| {
            let line = f.lines().find(|l| l.contains("Sending…")).unwrap();
            line[line.find("Sending…").unwrap()..].to_owned()
        })
        .collect();
    let at = Instant::now();
    tx.send(key("Esc")).unwrap();
    let (_, cancelled) = frame_after(frames, at, |f| f.contains("Cancelled after"));
    let at = Instant::now();
    tx.send(key("Ctrl+C")).unwrap();
    let (_, idle_hint) = frame_after(frames, at, |f| f.contains("Press Ctrl+Q to quit"));
    tx.send(key("Ctrl+Q")).unwrap();
    Seen {
        key_latencies,
        elapsed_texts,
        cancelled,
        idle_hint,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ui_stays_responsive_while_a_slow_request_is_pending_and_esc_cancels_it() {
    // Scenarios "UI stays responsive during a slow request", "Cancel a
    // hanging request" and "Ctrl+C when idle".
    let server = TestServer::http().await;
    let id = format!("tui-slow-{}", std::process::id());
    let tmp = workspace(&server, &id);
    let mut state = shell(&tmp, "slow.rman.yaml", 120, 30);
    state.shell.focus = Focus::Sidebar;
    let (tx, rx) = unbounded_channel();
    let (frames_tx, frames) = mpsc::channel();
    let backend = Recording {
        inner: TestBackend::new(120, 30),
        frames: frames_tx,
    };
    let mut terminal = Terminal::new(backend).unwrap();
    let user_tx = tx.clone();
    let quit_tx = tx.clone();
    let user = std::thread::spawn(move || {
        let seen =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| script(user_tx, &frames)));
        // A failed script still ends the loop, so the failure is reported.
        let _ = quit_tx.send(key("Ctrl+Q"));
        let _ = quit_tx.send(key("Ctrl+Q"));
        seen
    });
    let state = run_app(&mut terminal, state, Vec::new(), tx, rx, &mut Headless)
        .await
        .unwrap();
    let seen = user
        .join()
        .unwrap()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));

    assert_eq!(seen.key_latencies.len(), 12);
    // The spec's 50 ms per keypress, as a median so one descheduled CI
    // runner tick does not fail a debug build; a hard ceiling still
    // catches a loop that blocks.
    crate::testing::assert_timing(
        "keypress to screen",
        &seen.key_latencies,
        Duration::from_millis(50),
        Duration::from_millis(250),
    );
    assert_ne!(
        seen.elapsed_texts[0], seen.elapsed_texts[1],
        "the spinner and elapsed counter keep updating"
    );
    assert!(
        seen.cancelled.contains("Cancelled after"),
        "{}",
        seen.cancelled
    );
    assert!(seen.idle_hint.contains("Press Ctrl+Q to quit"));
    assert!(state.quit);
    assert_eq!(state.status.in_flight, 0);
    let received = state.tabs.active().unwrap().exchange.as_deref().unwrap();
    assert!(received.is_cancelled());

    // The connection was closed: the server saw the request go away long
    // before its 10 s were up.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let hits = timed_hits(&id);
        if hits.iter().any(|h| h.dropped.is_some()) {
            assert!(hits.iter().all(|h| h.end.is_none()), "never answered");
            break;
        }
        assert!(Instant::now() < deadline, "the server still holds {hits:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_json_response_is_shown_as_a_tree_with_its_history() {
    let server = TestServer::http().await;
    let tmp = workspace(&server, "unused");
    let mut state = shell(&tmp, "user.rman.yaml", 160, 45);
    send(&mut state).await;
    send(&mut state).await;
    let screen = text(&state);
    assert!(screen.contains("✔ 200 OK"), "{screen}");
    assert!(screen.contains("2/2 passed"), "{screen}");
    assert!(screen.contains("name: \"Ana\""), "{screen}");
    let tab = state.tabs.active_mut().unwrap();
    let received = tab.exchange.as_deref().unwrap();
    assert!(matches!(received.body, BodyKind::Json { .. }));
    assert_eq!(received.history.len(), 2, "both sends are in history");
    tab.viewer.set_tab(ResponseTab::Timing);
    let screen = text(&state);
    assert!(screen.contains("2 points"), "{screen}");
    assert!(screen.contains("Waiting"), "{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_echoed_by_the_server_never_reach_the_viewer() {
    // Canary for every surface of the response viewer: the body tree and
    // raw text, headers, captured values, the copy and the saved body.
    // Scenario "Echoed secret in a response": exchange data shows the
    // engine's marker, captured secrets the TUI's bullets.
    let server = TestServer::http().await;
    let tmp = workspace(&server, "unused");
    let mut state = shell(&tmp, "echo.rman.yaml", 200, 60);
    send(&mut state).await;
    let mut screens = Vec::new();
    for tab in ResponseTab::ALL {
        state.tabs.active_mut().unwrap().viewer.set_tab(tab);
        screens.push(text(&state));
    }
    let viewer = &mut state.tabs.active_mut().unwrap().viewer;
    viewer.set_tab(ResponseTab::Body);
    viewer.toggle_raw();
    screens.push(text(&state));
    let joined = screens.join("\n");
    assert!(joined.contains("✔ 200 OK"), "{joined}");
    assert!(joined.contains("«redacted:token»"), "{joined}");
    assert!(joined.contains("method = GET"), "{joined}");
    assert!(joined.contains("echoed = ••••••••"), "{joined}");
    assert!(!joined.contains(CANARY), "{joined}");

    let tab = state.tabs.active_mut().unwrap();
    tab.viewer.toggle_raw();
    let copied = tab.viewer.copy_text().unwrap();
    assert!(copied.contains("redacted") && !copied.contains(CANARY));
    let source = tab.exchange.as_ref().unwrap().source.clone().unwrap();
    let saved = tmp.path().join("saved.json");
    execute(Effect::SaveBody {
        source,
        path: saved.clone(),
    });
    let file = fs::read_to_string(&saved).unwrap();
    assert!(
        file.contains("redacted") && !file.contains(CANARY),
        "{file}"
    );
}

fn press(state: &mut State, code: KeyCode) -> Vec<Effect> {
    update(state, Action::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exchange_diff_runs_as_an_effect_and_stays_redacted() {
    // The diff parses and walks both bodies, so `Enter` in the picker hands
    // it to the effect runner instead of computing it in the key handler.
    // Canary for the picker and the diff overlay too.
    let server = TestServer::http().await;
    let tmp = workspace(&server, "unused");
    let mut state = shell(&tmp, "echo.rman.yaml", 160, 45);
    send(&mut state).await;
    send(&mut state).await;
    state.shell.focus = Focus::Response;
    let effects = press(&mut state, KeyCode::Char('D'));
    let [load @ Effect::LoadExchanges { .. }] = effects.as_slice() else {
        panic!("expected the history to be loaded off the loop");
    };
    assert!(update(&mut state, execute(load.clone())).is_empty());
    let picker_screen = text(&state);
    assert!(picker_screen.contains("Diff with"), "{picker_screen}");

    let effects = press(&mut state, KeyCode::Enter);
    let [diff @ Effect::Diff(_)] = effects.as_slice() else {
        panic!("expected a diff effect, got {effects:?}");
    };
    assert!(matches!(
        state.overlay,
        Some(crate::app::Overlay::Exchanges(_))
    ));
    assert!(text(&state).contains("Diffing"), "{}", text(&state));
    assert!(update(&mut state, execute(diff.clone())).is_empty());
    let diff_screen = text(&state);
    assert!(diff_screen.contains("Diff: "), "{diff_screen}");
    assert!(matches!(state.overlay, Some(crate::app::Overlay::Diff(_))));
    for screen in [&picker_screen, &diff_screen] {
        assert!(!screen.contains(CANARY), "{screen}");
    }

    // A diff that finishes after the picker was closed is dropped.
    press(&mut state, KeyCode::Esc);
    assert!(update(&mut state, execute(diff.clone())).is_empty());
    assert!(state.overlay.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spilled_body_is_spooled_redacted() {
    // Bodies above `max_body_bytes` are spilled by the engine; the viewer
    // keeps its own redacted spool of them (canary for the spool path).
    let server = TestServer::http().await;
    let tmp = workspace(&server, "unused");
    let root = tmp.path().join("ws");
    let settings = root.join("rustman.toml");
    let mut toml = fs::read_to_string(&settings).unwrap();
    toml.push_str("\n[http]\nmax_body_bytes = 1024\n");
    fs::write(&settings, toml).unwrap();
    fs::write(
        root.join("big.rman.yaml"),
        "version: 1\nkind: http\nname: Big\nmethod: POST\n\
         url: '{{base_url}}/mirror?pad=5000&tail=10'\n\
         body:\n  type: json\n  content: '\"{{token}}\"'\n",
    )
    .unwrap();
    let mut state = shell(&tmp, "big.rman.yaml", 160, 45);
    send(&mut state).await;
    let received = state.tabs.active().unwrap().exchange.as_deref().unwrap();
    let Some(crate::response::Source::Spool(spool)) = &received.source else {
        panic!("expected a spooled body: {:?}", received.source);
    };
    let kept = fs::read_to_string(spool.path()).unwrap();
    assert!(kept.starts_with("AAAA") && kept.ends_with("BBBB"), "{kept}");
    assert!(kept.contains("«redacted:token»"), "{kept}");
    assert!(!kept.contains(CANARY));
    let screen = text(&state);
    assert!(!screen.contains(CANARY), "{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_above_the_tree_limit_is_paged_through_the_real_engine() {
    // Scenario "80 MiB JSON response" end to end: a body above both the
    // engine's default `max_body_bytes` (10 MiB, so it is spilled) and the
    // tree limit goes through the engine, the viewer's redacted spool and
    // the paged view (the unit test uses a hand-built sparse spool).
    let server = TestServer::http().await;
    let tmp = workspace(&server, "unused");
    let limit = crate::response::received::TREE_LIMIT;
    let pad = limit + 1024 * 1024;
    fs::write(
        tmp.path().join("ws/huge.rman.yaml"),
        format!(
            "version: 1\nkind: http\nname: Huge\nmethod: POST\n\
             url: '{{{{base_url}}}}/mirror?pad={pad}&tail=10'\n\
             body:\n  type: json\n  content: '\"x\"'\n"
        ),
    )
    .unwrap();
    let mut state = shell(&tmp, "huge.rman.yaml", 160, 45);
    send(&mut state).await;
    let received = state.tabs.active().unwrap().exchange.as_deref().unwrap();
    assert!(received.size > limit, "{}", received.size);
    assert!(matches!(received.body, BodyKind::Paged));
    let source = received.source.clone().expect("a body");
    let crate::response::Source::Spool(spool) = &source else {
        panic!("expected a spooled body: {source:?}");
    };
    assert_eq!(fs::metadata(spool.path()).unwrap().len(), received.size);
    state.shell.focus = Focus::Response;
    let screen = text(&state);
    assert!(
        screen.contains("The tree view is disabled above 50 MiB"),
        "{screen}"
    );
    assert!(screen.contains("Page 1 of 817"), "{screen}");
    let tab = state.tabs.active().unwrap().id;
    let page = execute(Effect::ReadPage {
        tab,
        page: 0,
        source,
    });
    update(&mut state, page);
    let screen = text(&state);
    assert!(screen.contains(&"A".repeat(50)), "{screen}");
    assert!(!screen.contains("loading"), "{screen}");
}
