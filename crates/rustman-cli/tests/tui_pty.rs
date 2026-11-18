//! `rustman tui` in a real terminal: the no-workspace prompt, a scratch
//! session, `Ctrl+Q`, termination signals and the restored terminal.
//!
//! Unix only: Windows' ConPTY rewrites the output stream, so the escape
//! sequences checked here are not passed through verbatim (the full
//! cross-platform pty smoke test is task 9.1 of add-terminal-ui).
#![cfg(unix)]

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const WAIT: Duration = Duration::from_secs(30);

/// Collects pty output and waits for text to appear in it.
struct Screen {
    rx: Receiver<Vec<u8>>,
    seen: String,
}

impl Screen {
    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + WAIT;
        while !self.seen.contains(text) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(chunk) => self.seen.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => panic!("{text:?} never appeared in {:?}", self.seen),
            }
        }
    }
}

/// `rustman` running in an 80x24 pty.
struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    screen: Screen,
    // Keeps the pty open until the session ends.
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _config: tempfile::TempDir,
}

/// `rustman tui DIR`.
fn launch(dir: &Path) -> Session {
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.arg("tui");
    cmd.arg(dir);
    spawn(cmd, "")
}

/// Run `cmd` in a pty with an empty configuration directory, so the
/// developer's own `tui.toml` and `keybindings.toml` cannot change the
/// outcome; `tui_toml` becomes its `rustman/tui.toml`. The directory lives
/// as long as the session.
fn spawn(mut cmd: CommandBuilder, tui_toml: &str) -> Session {
    let config = tempfile::tempdir().expect("config dir");
    let dir = config.path().join("rustman");
    std::fs::create_dir_all(&dir).expect("config subdir");
    std::fs::write(dir.join("tui.toml"), tui_toml).expect("write tui.toml");
    cmd.env("XDG_CONFIG_HOME", config.path());
    cmd.env("APPDATA", config.path());
    cmd.env("HOME", config.path());
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let child = pty.slave.spawn_command(cmd).expect("spawn rustman");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("pty reader");
    let writer = pty.master.take_writer().expect("pty writer");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n @ 1..) = reader.read(&mut chunk) {
            if tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    Session {
        child,
        writer,
        screen: Screen {
            rx,
            seen: String::new(),
        },
        _master: pty.master,
        _config: config,
    }
}

/// Assert the terminal was put back: main screen, mouse released, cursor
/// shown, all after the UI was entered.
fn assert_restored(screen: &mut Screen) {
    // Each restore step is its own write, so wait for the last one.
    screen.wait_for("\x1b[?1049l");
    screen.wait_for("\x1b[?25h");
    let entered = screen.seen.find("\x1b[?1000h").expect("mouse captured");
    let after = &screen.seen[entered..];
    assert!(after.contains("\x1b[?1000l"), "mouse released: {after:?}");
    assert!(after.contains("\x1b[?25h"), "cursor shown: {after:?}");
}

#[test]
fn prompt_then_scratch_session_then_quit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = launch(tmp.path());
    let screen = &mut session.screen;

    // ratatui skips unchanged cells with cursor moves, so match single words.
    screen.wait_for("Create");
    screen.wait_for("Scratch");
    session.writer.write_all(b"s").unwrap();
    // The 80x24 shell stacks the request editor over the response viewer.
    screen.wait_for("Response");
    session.writer.write_all(b"\x11").unwrap(); // Ctrl+Q
    let status = session.child.wait().expect("rustman exits");
    assert!(status.success(), "{:?}", screen.seen);
    assert_restored(screen);
    assert!(!tmp.path().join("rustman.toml").exists());
}

#[test]
fn termination_signals_restore_the_terminal() {
    for (signal, code) in [("TERM", 143), ("HUP", 129), ("INT", 130)] {
        let tmp = tempfile::tempdir().unwrap();
        let mut session = launch(tmp.path());
        session.screen.wait_for("Scratch");
        let pid = session.child.process_id().expect("child pid");
        let killed = std::process::Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status()
            .expect("run kill");
        assert!(killed.success());
        let status = session.child.wait().expect("rustman exits");
        assert_eq!(status.exit_code(), code, "SIG{signal}");
        assert_restored(&mut session.screen);
    }
}

#[test]
fn bare_rustman_opens_the_workspace_found_above_the_cwd() {
    // Scenario "Open workspace from a subdirectory", with no subcommand.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    let sub = root.join("api").join("users");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"proj\"\nversion = 1\n",
    )
    .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.cwd(&sub);
    let mut session = spawn(cmd, "");
    session.screen.wait_for("proj");
    session.writer.write_all(b"\x11").unwrap(); // Ctrl+Q
    let status = session.child.wait().expect("rustman exits");
    assert!(status.success(), "{:?}", session.screen.seen);
    assert!(
        !session.screen.seen.contains("Scratch"),
        "{:?}",
        session.screen.seen
    );
    assert_restored(&mut session.screen);
}

#[test]
fn a_config_without_mouse_still_restores_the_terminal() {
    // Regression: restore must not depend on the mouse ever being captured.
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.arg("tui");
    cmd.arg(tmp.path());
    let mut session = spawn(cmd, "mouse = false\n");
    let screen = &mut session.screen;
    screen.wait_for("Scratch");
    session.writer.write_all(b"\x11").unwrap();
    let status = session.child.wait().expect("rustman exits");
    assert!(status.success(), "{:?}", screen.seen);
    screen.wait_for("\x1b[?1049l");
    screen.wait_for("\x1b[?25h");
    let entered = screen.seen.find("\x1b[?1049h").expect("alternate screen");
    let after = &screen.seen[entered..];
    assert!(!after.contains("\x1b[?1000h"), "mouse captured: {after:?}");
    assert!(after.contains("\x1b[?25h"), "cursor shown: {after:?}");
}
