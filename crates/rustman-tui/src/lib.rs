//! Rustman's terminal user interface (openspec change `add-terminal-ui`).
//!
//! The UI follows an Elm-style loop (design D2): terminal input and effect
//! completions become [`app::Action`]s on one channel, [`app::update`]
//! changes the [`app::State`] synchronously and returns [`app::Effect`]s, an
//! effect runner task performs the I/O, and [`view::view`] draws the state.

pub mod app;
mod commands;
pub mod config;
pub mod effects;
pub mod focus;
pub mod glyphs;
pub mod help;
mod input;
pub mod keymap;
pub mod launch;
pub mod layout;
pub mod palette;
mod runtime;
pub mod shell;
mod shell_view;
mod signals;
pub mod snapshot;
pub mod status;
pub mod terminal;
pub mod textarea;
pub mod theme;
pub mod toast;
pub mod view;
pub mod vim;

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
/// the config directory, the command-line `overrides` and the terminal's
/// colour mode. Problems in either file become warning toasts.
pub fn initial_state(start: &Path, overrides: Overrides) -> (State, Vec<app::Effect>) {
    let dir = config::config_dir();
    initial_state_with(start, overrides, dir.as_deref(), ColorMode::from_env())
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
    let mut guard = TerminalGuard::enter_with(state.ui.mouse)?;
    let stop = CancellationToken::new();
    let reader = input::spawn_reader(tx.clone(), stop.clone());
    let result = rt.block_on(runtime::run_app(guard.terminal(), state, effects, tx, rx));
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
