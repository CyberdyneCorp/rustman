//! A panic inside the TUI restores the terminal before the panic message is
//! printed (spec: Launching the TUI, scenario "Panic restores the terminal").
//!
//! The test re-runs this test binary under a pseudo-terminal. The child enters
//! the TUI, records the line discipline with `stty -a`, panics, and records it
//! again. Unix only: on Windows the ConPTY host does not expose the child's
//! console mode, so cooked mode cannot be observed from outside, and there is
//! no `stty`. On Windows crossterm drives the console API instead of escape
//! sequences; `terminal::restore_screen` has its own Windows unit test.
#![cfg(unix)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const CHILD_ENV: &str = "RUSTMAN_TUI_PANIC_CHILD";
const PANIC_TEXT: &str = "deliberate panic inside the TUI";

fn stty() -> String {
    let out = Command::new("stty")
        .arg("-a")
        .stdin(Stdio::inherit())
        .output()
        .expect("run stty");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has_flag(stty: &str, flag: &str) -> bool {
    stty.split_whitespace().any(|word| word == flag)
}

/// The child side; does nothing unless run by `panic_restores_the_terminal`.
#[test]
fn panic_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let result = std::panic::catch_unwind(|| {
        let _guard = rustman_tui::terminal::TerminalGuard::enter().expect("enter the TUI");
        println!("\r\nIN-TUI-STTY[{}]", stty().replace('\n', " "));
        panic!("{PANIC_TEXT}");
    });
    assert!(result.is_err());
    println!("AFTER-STTY[{}]", stty().replace('\n', " "));
}

fn between<'a>(text: &'a str, start: &str) -> &'a str {
    let from = text
        .find(start)
        .unwrap_or_else(|| panic!("{start} missing in:\n{text}"))
        + start.len();
    let len = text[from..].find(']').expect("closing bracket");
    &text[from..from + len]
}

fn run_child_under_pty() -> String {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut cmd = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["panic_child", "--exact", "--nocapture", "--test-threads=1"]);
    cmd.env(CHILD_ENV, "1");
    let mut child = pty.slave.spawn_command(cmd).expect("spawn the child");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("pty reader");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        // EOF, or EIO once the child side is closed, ends the output.
        while let Ok(n @ 1..) = reader.read(&mut chunk) {
            out.extend_from_slice(&chunk[..n]);
        }
        let _ = tx.send(out);
    });
    let status = child.wait().expect("wait for the child");
    drop(pty.master);
    let out = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("child output");
    assert!(
        status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&out)
    );
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn panic_restores_the_terminal() {
    let out = run_child_under_pty();
    let in_tui = between(&out, "IN-TUI-STTY[");
    assert!(
        has_flag(in_tui, "-icanon") && has_flag(in_tui, "-echo"),
        "not raw in the TUI: {in_tui}"
    );
    let after = between(&out, "AFTER-STTY[");
    assert!(
        has_flag(after, "icanon") && has_flag(after, "echo"),
        "not cooked after the panic: {after}"
    );

    let panic_at = out.find(PANIC_TEXT).expect("panic message printed");
    let alt_screen = out.find("\x1b[?1049h").expect("alternate screen entered");
    let main_screen = out.rfind("\x1b[?1049l").expect("main screen restored");
    let cursor = out.rfind("\x1b[?25h").expect("cursor shown");
    assert!(alt_screen < main_screen, "{out:?}");
    // The hook restores before the message, so a restore precedes it.
    let restored_first =
        out[..panic_at].contains("\x1b[?1049l") && out[..panic_at].contains("\x1b[?25h");
    assert!(
        restored_first,
        "terminal restored after the panic message: {out:?}"
    );
    assert!(main_screen > alt_screen && cursor > alt_screen);
}
