//! The smoke scenario of task 9.1 through the real event loop, effect
//! runner and engine, with an in-memory 80x24 terminal: open a fixture
//! request from the palette, send it to the local mock, cancel a slow one,
//! quit. `tests/pty_smoke.rs` runs the same flow in a pseudo-terminal
//! (ConPTY on Windows); this one needs no console, so it tells a runner's
//! console problem apart from a TUI regression (see docs/tui.md, "Platform
//! checks").

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rustman_core::SecretBackend;
use rustman_test_support::TestServer;
use rustman_test_support::routes::timed_hits;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::app::Action;
use crate::config::Overrides;
use crate::env_tests::workspace;
use crate::runtime::{Headless, run_app};
use crate::send_tests::{Recording, frame_after, key};
use crate::theme::ColorMode;

fn type_text(tx: &UnboundedSender<Action>, text: &str) {
    for c in text.chars() {
        tx.send(key(&c.to_string())).unwrap();
    }
}

/// Open `file` from the command palette and wait until its tab shows.
fn open(tx: &UnboundedSender<Action>, frames: &mpsc::Receiver<(Instant, String)>, file: &str) {
    let at = Instant::now();
    tx.send(key("Ctrl+P")).unwrap();
    frame_after(frames, at, |f| f.contains("Command palette"));
    type_text(tx, file);
    tx.send(key("Enter")).unwrap();
    let title = format!("{} ", file.trim_end_matches(".rman.yaml"));
    frame_after(frames, at, |f| {
        f.contains(&title) && !f.contains("Command palette")
    });
}

/// What the scripted user saw.
struct Seen {
    sent: String,
    cancelled: String,
}

fn script(
    tx: &UnboundedSender<Action>,
    frames: &mpsc::Receiver<(Instant, String)>,
    slow_id: &str,
) -> Seen {
    let start = Instant::now();
    frame_after(frames, start, |f| f.contains("env: local"));
    open(tx, frames, "fast.rman.yaml");
    let at = Instant::now();
    tx.send(key("Ctrl+R")).unwrap();
    let (_, sent) = frame_after(frames, at, |f| f.contains("200 OK"));
    open(tx, frames, "slow.rman.yaml");
    let at = Instant::now();
    tx.send(key("Ctrl+R")).unwrap();
    frame_after(frames, at, |f| f.contains("1 in flight"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while timed_hits(slow_id).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let at = Instant::now();
    tx.send(key("Esc")).unwrap();
    let (_, cancelled) = frame_after(frames, at, |f| f.contains("Cancelled after"));
    tx.send(key("Ctrl+Q")).unwrap();
    Seen { sent, cancelled }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn smoke_open_send_cancel_quit_at_80x24() {
    let server = TestServer::http().await;
    let slow_id = format!("loop-smoke-{}", std::process::id());
    let env = format!("variables:\n  base_url: {}\n", server.url(""));
    let slow = format!(
        "version: 1\nkind: http\nname: Slow\nmethod: GET\nurl: '{{{{base_url}}}}/timed/60000?id={slow_id}'\n"
    );
    let tmp = workspace(&[
        (
            "rustman.toml",
            "[workspace]\nname = \"smoke\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n",
        ),
        ("environments/local.yaml", &env),
        (
            "fast.rman.yaml",
            "version: 1\nkind: http\nname: Fast\nmethod: GET\nurl: '{{base_url}}/echo?smoke=1'\n",
        ),
        ("slow.rman.yaml", &slow),
    ]);
    let config = tmp.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &tmp.path().join("ws"),
        Overrides::default(),
        Some(&config),
        ColorMode::NoColor,
    );
    state.secret_backend = Some(SecretBackend::File);
    let (tx, rx) = unbounded_channel();
    let (frames_tx, frames) = mpsc::channel();
    let backend = Recording {
        inner: TestBackend::new(80, 24),
        frames: frames_tx,
    };
    let mut terminal = Terminal::new(backend).unwrap();
    let user_tx = tx.clone();
    let id = slow_id.clone();
    let user = std::thread::spawn(move || script(&user_tx, &frames, &id));
    let state = run_app(&mut terminal, state, effects, tx, rx, &mut Headless)
        .await
        .unwrap();
    let seen = user.join().unwrap();

    assert!(state.quit);
    assert!(seen.sent.contains("200 OK"), "{}", seen.sent);
    assert!(
        seen.cancelled.contains("Cancelled after"),
        "{}",
        seen.cancelled
    );
    assert_eq!(state.status.in_flight, 0);
    let deadline = Instant::now() + Duration::from_secs(20);
    while timed_hits(&slow_id)[0].dropped.is_none() {
        assert!(
            Instant::now() < deadline,
            "the server never saw the connection close"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
