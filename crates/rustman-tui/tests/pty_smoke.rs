//! Cross-platform smoke test of the real TUI under a pseudo-terminal
//! (add-terminal-ui task 9.1): at 80x24, open a fixture request, send it to
//! a local mock, cancel a slow one, quit, and check the terminal is given
//! back. The child is this test binary running `rustman_tui::run`, so the
//! real terminal setup, input reader, effect runner and engine are used;
//! the parent types keys into the pty and reads the screen through a
//! terminal emulator (`vt100`).
//!
//! Linux and macOS use a Unix pty; Windows uses ConPTY (`portable-pty`
//! opens it with `PSEUDOCONSOLE_INHERIT_CURSOR`, so the parent answers its
//! cursor position query). crossterm turns on VT processing on Windows 10
//! and later and writes the same escape sequences there, but ConPTY
//! re-renders what the child writes, so the exact alternate screen bytes
//! are only checked on Unix; on every OS the emulated screen must be back
//! to the main screen once the TUI quits.

use std::fs;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rustman_test_support::TestServer;
use rustman_test_support::routes::timed_hits;

const CHILD_ENV: &str = "RUSTMAN_TUI_SMOKE_CHILD";
const ROWS: u16 = 24;
const COLS: u16 = 80;
/// Generous: debug builds on busy CI runners.
const WAIT: Duration = Duration::from_secs(30);

/// The child side; does nothing unless run by `smoke_under_a_pty`.
#[test]
fn smoke_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    rustman_tui::run(Path::new(&dir), rustman_tui::config::Overrides::default())
        .expect("the TUI runs and quits cleanly");
}

/// The fixture workspace: `fast` answers at once, `slow` after 60 s.
fn workspace(dir: &Path, server: &TestServer, slow_id: &str) {
    let write = |rel: &str, text: String| {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"smoke\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n"
            .into(),
    );
    write(
        "environments/local.yaml",
        format!("variables:\n  base_url: {}\n", server.url("")),
    );
    write(
        "fast.rman.yaml",
        "version: 1\nkind: http\nname: Fast\nmethod: GET\nurl: '{{base_url}}/echo?smoke=1'\n"
            .into(),
    );
    write(
        "slow.rman.yaml",
        format!(
            "version: 1\nkind: http\nname: Slow\nmethod: GET\nurl: '{{{{base_url}}}}/timed/60000?id={slow_id}'\n"
        ),
    );
}

/// ConPTY's cursor position query, sent before any output when it is told
/// to inherit the cursor.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// The child under a pty, its screen and everything it wrote.
struct Session {
    writer: Writer,
    screen: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<Vec<u8>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Session {
    fn start(workspace: &Path, home: &Path) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open a pty");
        let mut cmd = CommandBuilder::new(std::env::current_exe().expect("test binary path"));
        cmd.args(["smoke_child", "--exact", "--nocapture", "--test-threads=1"]);
        cmd.env(CHILD_ENV, workspace);
        cmd.env("HOME", home);
        cmd.env("USERPROFILE", home);
        cmd.env("XDG_CONFIG_HOME", home.join("config"));
        cmd.env("APPDATA", home.join("config"));
        cmd.env("LOCALAPPDATA", home.join("local"));
        cmd.env("RUSTMAN_SECRET_STORE", "file");
        cmd.env("TERM", "xterm-256color");
        cmd.env_remove("RUSTMAN_VIZ");
        let child = pty.slave.spawn_command(cmd).expect("spawn the child");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let writer: Writer = Arc::new(Mutex::new(pty.master.take_writer().expect("pty writer")));
        let rows = NonZeroU16::new(ROWS).unwrap();
        let cols = NonZeroU16::new(COLS).unwrap();
        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let (screen_in, raw_in, answer) = (screen.clone(), raw.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            // EOF, or EIO once the child side is closed, ends the output.
            while let Ok(n @ 1..) = reader.read(&mut chunk) {
                let output = &chunk[..n];
                if output
                    .windows(CURSOR_QUERY.len())
                    .any(|w| w == CURSOR_QUERY)
                {
                    let mut answer = answer.lock().unwrap();
                    let _ = answer.write_all(b"\x1b[1;1R");
                    let _ = answer.flush();
                }
                screen_in.lock().unwrap().process(output);
                raw_in.lock().unwrap().extend_from_slice(output);
            }
        });
        Self {
            writer,
            screen,
            raw,
            child,
            _master: pty.master,
        }
    }

    fn contents(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    /// Wait until the screen shows `text`.
    fn wait_for(&self, text: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let screen = self.contents();
            if screen.contains(text) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "`{text}` not on screen within {WAIT:?}:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn keys(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
        drop(writer);
        // Let the reader see one key sequence at a time (a lone `Esc` must
        // not merge with what follows).
        std::thread::sleep(Duration::from_millis(150));
    }

    /// Open a request from the command palette.
    fn open(&mut self, file: &str) {
        self.keys(b"\x10"); // Ctrl+P
        self.wait_for("Command palette");
        self.keys(file.as_bytes());
        self.keys(b"\r");
        self.wait_for(&format!("{} ", file.trim_end_matches(".rman.yaml")));
    }

    fn wait_exit(&mut self) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("child status") {
                return status.success();
            }
            assert!(Instant::now() < deadline, "the TUI did not quit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn smoke_under_a_pty() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(TestServer::http());
    let slow_id = format!("pty-smoke-{}", std::process::id());
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    workspace(&ws, &server, &slow_id);
    let mut tui = Session::start(&ws, tmp.path());

    let first = tui.wait_for("env: local");
    assert!(first.contains("smoke"), "{first}");

    tui.open("fast.rman.yaml");
    tui.keys(b"\x12"); // Ctrl+R
    tui.wait_for("200 OK");

    tui.open("slow.rman.yaml");
    tui.keys(b"\x12");
    tui.wait_for("1 in flight");
    let deadline = Instant::now() + WAIT;
    while timed_hits(&slow_id).is_empty() {
        assert!(Instant::now() < deadline, "the slow request never arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
    tui.keys(b"\x1b"); // Esc
    tui.wait_for("Cancelled after");
    tui.wait_for("0 in flight");
    let deadline = Instant::now() + WAIT;
    while timed_hits(&slow_id)[0].dropped.is_none() {
        assert!(
            Instant::now() < deadline,
            "the server never saw the connection close"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    tui.keys(b"\x11"); // Ctrl+Q
    let success = tui.wait_exit();
    assert!(success, "the TUI failed:\n{}", tui.contents());
    let deadline = Instant::now() + WAIT;
    while tui.contents().contains("env: local") {
        assert!(
            Instant::now() < deadline,
            "the main screen was not restored:\n{}",
            tui.contents()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !tui.screen.lock().unwrap().screen().hide_cursor(),
        "the cursor is shown again"
    );
    if cfg!(unix) {
        let raw = String::from_utf8_lossy(&tui.raw.lock().unwrap()).into_owned();
        let alt = raw.find("\x1b[?1049h").expect("alternate screen entered");
        let main = raw.rfind("\x1b[?1049l").expect("main screen restored");
        assert!(alt < main);
        assert!(raw.rfind("\x1b[?25h").expect("cursor shown") > alt);
    }
}
