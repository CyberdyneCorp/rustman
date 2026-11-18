//! Rustman's terminal user interface (openspec change `add-terminal-ui`).
//!
//! The UI follows an Elm-style loop (design D2): terminal input and effect
//! completions become [`app::Action`]s on one channel, [`app::update`]
//! changes the [`app::State`] synchronously and returns [`app::Effect`]s, an
//! effect runner task performs the I/O, and [`view::view`] draws the state.

pub mod app;
pub mod clipboard;
mod commands;
pub mod config;
pub mod dialogs;
pub mod effects;
#[cfg(test)]
mod env_tests;
pub mod external;
pub mod files;
pub mod focus;
pub mod glyphs;
pub mod help;
pub mod http_pane;
mod http_view;
mod input;
pub mod keymap;
pub mod kv;
pub mod launch;
pub mod layout;
pub mod palette;
pub mod pane;
pub mod response;
pub mod reveal;
mod runtime;
pub mod send;
#[cfg(test)]
mod send_tests;
pub mod shell;
mod shell_view;
mod signals;
#[cfg(test)]
mod smoke_tests;
pub mod snapshot;
pub mod status;
pub mod tabs;
pub mod terminal;
pub mod textarea;
pub mod theme;
pub mod toast;
pub mod tree;
pub mod vars_view;
pub mod view;
mod viewer_keys;
pub mod vim;
pub mod viz;
#[cfg(test)]
mod viz_tests;
pub mod watch;
pub mod webhook;
pub mod workbench;
#[cfg(test)]
mod workbench_tests;
mod workbench_view;

#[cfg(test)]
pub(crate) mod testing {
    //! Helpers shared by the unit tests.

    use crate::app::State;
    use crate::focus::Focus;

    /// Open a new tab with a raw JSON body and focus the body.
    pub fn open_raw_body(state: &mut State) {
        crate::workbench::new_tab(state);
        let tab = state.tabs.active_mut().expect("just opened");
        tab.editor.set_tab(2);
        tab.editor.cycle_mode();
        state.shell.focus = Focus::EditorBody;
    }

    /// A fake external editor that replaces the file with
    /// `{"edited":true}`; returns its path.
    pub fn fake_editor(dir: &std::path::Path) -> String {
        if cfg!(windows) {
            let script = dir.join("fake-editor.cmd");
            std::fs::write(&script, "@echo {\"edited\":true}> %1\r\n").unwrap();
            return script.display().to_string();
        }
        let script = dir.join("fake-editor");
        std::fs::write(&script, "#!/bin/sh\nprintf '{\"edited\":true}' > \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script.display().to_string()
    }

    /// Assert a timing budget robustly: the median of `samples` within
    /// `median`, and none above `max`. One slow sample (a CI runner
    /// descheduling the test) does not fail; a consistently slow path does.
    pub fn assert_timing(
        what: &str,
        samples: &[std::time::Duration],
        median: std::time::Duration,
        max: std::time::Duration,
    ) {
        assert!(!samples.is_empty(), "{what}: no samples");
        let mut sorted = samples.to_vec();
        sorted.sort();
        let mid = sorted[sorted.len() / 2];
        let worst = sorted[sorted.len() - 1];
        assert!(
            mid <= median,
            "{what}: median {mid:?} above {median:?} (all: {samples:?})"
        );
        assert!(
            worst <= max,
            "{what}: slowest {worst:?} above {max:?} (all: {samples:?})"
        );
    }

    #[test]
    fn timing_tolerates_one_outlier_but_not_a_slow_median() {
        use std::time::Duration;
        let ms = Duration::from_millis;
        let fine = [ms(5), ms(6), ms(200), ms(4), ms(5)];
        assert_timing("ok", &fine, ms(50), ms(250));
        let slow = [ms(60), ms(70), ms(80)];
        let failed = std::panic::catch_unwind(|| assert_timing("slow", &slow, ms(50), ms(250)));
        assert!(failed.is_err());
        let spike = [ms(5), ms(300), ms(5)];
        let failed = std::panic::catch_unwind(|| assert_timing("spike", &spike, ms(50), ms(250)));
        assert!(failed.is_err());
    }

    /// The raw body of the current tab.
    pub fn body_text(state: &State) -> String {
        state
            .tabs
            .active()
            .and_then(|t| t.editor.external_text())
            .map(|(text, _)| text)
            .unwrap_or_default()
    }
}

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::app::{Abort, State};
use crate::config::{Overrides, TuiConfig, UiOptions};
use crate::keymap::{Keymap, UserLayer};
use crate::terminal::TerminalGuard;
use crate::theme::ColorMode;
use crate::toast::Severity;

/// Why the TUI could not run.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("the terminal UI needs an interactive terminal (stdin and stdout must be a TTY)")]
    NotATerminal,
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("terminal error: {0}")]
    Io(#[from] io::Error),
    /// Stopped by a termination signal; `code` is the exit status to use.
    #[error("stopped by {name}")]
    Signal { name: &'static str, code: u8 },
    #[error("{0}")]
    Failed(String),
}

impl From<Abort> for TuiError {
    fn from(abort: Abort) -> Self {
        match abort {
            Abort::Signal { name, code } => Self::Signal { name, code },
            Abort::Failed(message) => Self::Failed(message),
        }
    }
}

/// True when stdin and stdout are both terminals.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// The initial state for `start`: `tui.toml` and `keybindings.toml` from
/// the config directory, the command-line `overrides`, the terminal's
/// colour mode and where to look for the viz companion. Problems in either
/// file become warning toasts.
pub fn initial_state(start: &Path, overrides: Overrides) -> (State, Vec<app::Effect>) {
    let dir = config::config_dir();
    let (mut state, effects) =
        initial_state_with(start, overrides, dir.as_deref(), ColorMode::from_env());
    state.viz = viz::Discovery::from_env();
    (state, effects)
}

/// [`initial_state`] with an explicit config directory and colour mode.
pub fn initial_state_with(
    start: &Path,
    overrides: Overrides,
    config_dir: Option<&Path>,
    mode: ColorMode,
) -> (State, Vec<app::Effect>) {
    let (file, warning) = TuiConfig::load(config_dir);
    let (user, key_warnings) = UserLayer::load(config_dir);
    let ui = UiOptions::resolve(&file, overrides, mode);
    let (keymap, conflicts) = Keymap::checked(ui.vim, &user);
    let (state, effects) = State::new(launch::discover(start));
    let mut state = state.with_ui(ui).with_keymap(keymap);
    for warning in warning.into_iter().chain(key_warnings).chain(conflicts) {
        state.notify(Severity::Warn, warning);
    }
    (state, effects)
}

/// The real terminal, handed over to the external editor and taken back.
struct TerminalHost {
    gate: input::Gate,
    mouse: bool,
}

impl runtime::Host for TerminalHost {
    fn suspend(&mut self) -> io::Result<()> {
        self.gate.set_paused(true);
        terminal::suspend(self.mouse)
    }

    fn resume(&mut self) -> io::Result<()> {
        let resumed = terminal::resume(self.mouse);
        self.gate.set_paused(false);
        resumed
    }

    fn write_escape(&mut self, sequence: &str) {
        use std::io::Write;
        let mut out = io::stdout();
        let _ = out
            .write_all(sequence.as_bytes())
            .and_then(|()| out.flush());
    }
}

/// Drop the runtime without waiting for blocking work still running (a
/// workspace load on a slow filesystem), so quitting is never delayed.
fn shut_down(rt: tokio::runtime::Runtime) {
    rt.shutdown_background();
}

/// Open the TUI on the workspace found from `start` (spec: Launching the
/// TUI) and run it until the user quits.
pub fn run(start: &Path, overrides: Overrides) -> Result<(), TuiError> {
    if !start.is_dir() {
        return Err(TuiError::NotADirectory(start.to_path_buf()));
    }
    if !is_interactive() {
        return Err(TuiError::NotATerminal);
    }
    // Spilled viz bodies go when this returns or a panic unwinds out of it.
    let _viz_spills = viz::CleanupGuard::new();
    let (state, effects) = initial_state(start, overrides);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Unbounded: the loop drains every queued action before each draw, and
    // nothing here produces actions faster than that. Revisit with bounded
    // channels when responses stream through them (task groups 5 and 7).
    let (tx, rx) = mpsc::unbounded_channel();
    {
        let _context = rt.enter();
        signals::spawn(&tx)?;
    }
    let mouse = state.ui.mouse;
    let mut guard = TerminalGuard::enter_with(mouse)?;
    let stop = CancellationToken::new();
    let gate = input::Gate::default();
    let reader = input::spawn_reader(tx.clone(), stop.clone(), gate.clone());
    let mut host = TerminalHost { gate, mouse };
    let result = rt.block_on(runtime::run_app(
        guard.terminal(),
        state,
        effects,
        tx,
        rx,
        &mut host,
    ));
    stop.cancel();
    let _ = reader.join();
    drop(guard);
    shut_down(rt);
    match result?.abort {
        Some(abort) => Err(abort.into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn quitting_does_not_wait_for_blocking_work() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.spawn_blocking(|| std::thread::sleep(Duration::from_secs(30)));
        let started = Instant::now();
        super::shut_down(rt);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
