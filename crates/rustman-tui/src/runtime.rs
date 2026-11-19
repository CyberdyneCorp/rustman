//! The application loop: draw, wait for the next action, update, dispatch
//! effects (design D2). Nothing here awaits network or disk I/O. The one
//! effect run in the loop itself is the external editor, while the terminal
//! is handed over to it (design D9).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app::{Action, Effect, State, update};
use crate::clipboard;
use crate::effects;
use crate::external;
use crate::toast::Severity;
use crate::view::view;
use crate::watch::{self, Watcher};
use crate::workbench;

/// What the loop needs from the real terminal besides drawing.
pub trait Host {
    /// Hand the terminal to another program (main screen, cooked mode, no
    /// input reading).
    fn suspend(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    /// Take the terminal back.
    fn resume(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    /// An environment variable (`VISUAL`, `EDITOR`).
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    /// Write an escape sequence (OSC 52 clipboard) to the terminal.
    fn write_escape(&mut self, _sequence: &str) {}
}

/// A host with no terminal to hand over (tests).
#[cfg(test)]
pub struct Headless;

#[cfg(test)]
impl Host for Headless {}

/// Run until the state asks to quit. `tx` is the sending side of the single
/// action channel; the effect runner reports completions on it.
pub async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut state: State,
    initial: Vec<Effect>,
    tx: UnboundedSender<Action>,
    mut rx: UnboundedReceiver<Action>,
    host: &mut dyn Host,
) -> Result<State, B::Error> {
    let size = terminal.size()?;
    update(
        &mut state,
        Action::Resize {
            width: size.width,
            height: size.height,
        },
    );
    spawn_ticker(tx.clone());
    let runner = effects::spawn_runner(tx.clone());
    let mut watcher = None;
    let mut hand_over = dispatch(&runner, initial);
    while !state.quit {
        while let Some(effect) = hand_over.pop() {
            let action = run_in_terminal(host, effect);
            terminal.clear()?;
            hand_over.extend(dispatch(&runner, update(&mut state, action)));
        }
        sync_watcher(&mut watcher, &state, &tx);
        if let Some(text) = state.clipboard.take() {
            host.write_escape(&clipboard::osc52(&text));
        }
        terminal.draw(|frame| view(&state, frame))?;
        let Some(action) = rx.recv().await else {
            break;
        };
        hand_over.extend(dispatch(&runner, update(&mut state, action)));
        // Apply everything already queued before drawing again (keys typed
        // after `Ctrl+E` wait for the external editor).
        while !state.quit && hand_over.is_empty() {
            let Ok(action) = rx.try_recv() else { break };
            hand_over.extend(dispatch(&runner, update(&mut state, action)));
        }
    }
    // Requests still in flight stop with the loop: their connections close.
    workbench::cancel_all(&mut state);
    crate::webhook::shut_down(&mut state);
    crate::mock_server::shut_down(&mut state);
    Ok(state)
}

/// Run an effect that needs the terminal: the external editor.
fn run_in_terminal(host: &mut dyn Host, effect: Effect) -> Action {
    let Effect::EditExternally {
        tab,
        text,
        extension,
    } = effect
    else {
        return effects::execute(effect);
    };
    let result = host
        .suspend()
        .and_then(|()| external::edit(&text, extension, |name| host.var(name)));
    let resumed = host.resume();
    let result = result.and_then(|text| resumed.map(|()| text));
    Action::ExternalEdited {
        tab,
        result: result.map_err(|e| e.to_string()),
    }
}

/// Keep a file watcher on the open workspace (none without one).
fn sync_watcher(watcher: &mut Option<Watcher>, state: &State, tx: &UnboundedSender<Action>) {
    let root: Option<PathBuf> = workbench::workspace(state).map(|ws| ws.root.clone());
    if watcher.as_ref().map(|w| &w.root) == root.as_ref() {
        return;
    }
    *watcher = None;
    let Some(root) = root else { return };
    match watch::watch(&root, tx.clone()) {
        Ok(started) => *watcher = Some(started),
        Err(err) => {
            let message = format!("Changes on disk are not followed: {err}");
            let _ = tx.send(Action::Notify(Severity::Warn, message));
        }
    }
}

/// How often the clock ticks: toast expiry, the send spinner and its
/// elapsed time.
const TICK: Duration = Duration::from_millis(100);

/// Send [`Action::Tick`] until the loop is gone.
fn spawn_ticker(tx: UnboundedSender<Action>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if tx.send(Action::Tick(Instant::now())).is_err() {
                break;
            }
        }
    });
}

/// Send effects to the runner; returns those that need the terminal.
fn dispatch(runner: &UnboundedSender<Effect>, effects: Vec<Effect>) -> Vec<Effect> {
    let mut hand_over = Vec::new();
    for effect in effects {
        if matches!(effect, Effect::EditExternally { .. }) {
            hand_over.push(effect);
        } else {
            // The runner lives as long as this loop, so a send cannot fail.
            let _ = runner.send(effect);
        }
    }
    hand_over
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Screen, Session};
    use crate::launch::Launch;
    use crate::snapshot::buffer_text;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc::unbounded_channel;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Action {
        Action::Key(KeyEvent::new(code, modifiers))
    }

    #[tokio::test]
    async fn the_loop_applies_queued_actions_and_quits() {
        let (state, effects) = State::new(Launch::NoWorkspace("/nowhere".into()));
        let (tx, rx) = unbounded_channel();
        for action in [
            key(KeyCode::Down, KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::NONE),
            Action::Resize {
                width: 80,
                height: 24,
            },
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
        ] {
            tx.send(action).unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let state = run_app(&mut terminal, state, effects, tx, rx, &mut Headless)
            .await
            .unwrap();
        assert!(state.quit);
        assert!(matches!(state.screen, Screen::Shell(Session::Scratch)));
    }

    #[tokio::test]
    async fn the_first_frame_is_drawn_before_any_input() {
        let (state, effects) = State::new(Launch::NoWorkspace("/nowhere".into()));
        let (tx, rx) = unbounded_channel();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        // The quit is already queued, yet the loop draws once before it.
        tx.send(key(KeyCode::Char('q'), KeyModifiers::CONTROL))
            .unwrap();
        run_app(&mut terminal, state, effects, tx, rx, &mut Headless)
            .await
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Create workspace here"), "{text}");
    }

    #[tokio::test]
    async fn keyboard_and_mouse_events_drive_the_rendered_shell() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mouse = |kind, column| {
            Action::Mouse(MouseEvent {
                kind,
                column,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })
        };
        let state = State::with_screen(Screen::Shell(Session::Scratch));
        let (tx, rx) = unbounded_channel();
        for action in [
            key(KeyCode::Tab, KeyModifiers::NONE),
            mouse(MouseEventKind::Down(MouseButton::Left), 27),
            mouse(MouseEventKind::Drag(MouseButton::Left), 35),
            mouse(MouseEventKind::Up(MouseButton::Left), 35),
            mouse(MouseEventKind::Down(MouseButton::Left), 150),
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
        ] {
            tx.send(action).unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        let state = run_app(&mut terminal, state, Vec::new(), tx, rx, &mut Headless)
            .await
            .unwrap();
        assert_eq!(state.shell.sidebar_width, 36);
        assert_eq!(state.shell.focus, crate::focus::Focus::Response);
        // The last frame was drawn before the quit was applied.
        terminal.draw(|frame| view(&state, frame)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(
            text.lines().next().unwrap().contains("┐┌ Request"),
            "{text}"
        );
        assert_eq!(&text[..text.find('┐').unwrap()].chars().count(), &35);
        assert!(text.contains("› Response"), "{text}");
    }

    /// A terminal host whose editor is a fake script.
    struct FakeTerminal {
        editor: String,
        handed_over: usize,
        taken_back: usize,
    }

    impl Host for FakeTerminal {
        fn suspend(&mut self) -> std::io::Result<()> {
            self.handed_over += 1;
            Ok(())
        }

        fn resume(&mut self) -> std::io::Result<()> {
            self.taken_back += 1;
            Ok(())
        }

        fn var(&self, name: &str) -> Option<String> {
            (name == "VISUAL").then(|| self.editor.clone())
        }
    }

    #[tokio::test]
    async fn ctrl_e_hands_the_terminal_to_the_editor_and_loads_its_edit() {
        // Scenario "Edit JSON body externally".
        let tools = tempfile::tempdir().unwrap();
        let editor = crate::testing::fake_editor(tools.path());
        let mut host = FakeTerminal {
            editor: shell_words::quote(&editor).into_owned(),
            handed_over: 0,
            taken_back: 0,
        };
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        crate::testing::open_raw_body(&mut state);
        let (tx, rx) = unbounded_channel();
        for action in [
            key(KeyCode::Char('e'), KeyModifiers::CONTROL),
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Action::Abort(crate::app::Abort::Failed("end of test".into())),
        ] {
            tx.send(action).unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let state = run_app(&mut terminal, state, Vec::new(), tx.clone(), rx, &mut host)
            .await
            .unwrap();
        assert_eq!((host.handed_over, host.taken_back), (1, 1));
        assert_eq!(crate::testing::body_text(&state), "{\"edited\":true}");
        assert!(
            state.tabs.active().unwrap().dirty,
            "the edit marks the tab dirty"
        );
        assert!(
            matches!(state.overlay, Some(crate::app::Overlay::Confirm(_))),
            "Ctrl+Q asks first: the tab is dirty"
        );
    }

    #[tokio::test]
    async fn the_loop_learns_the_terminal_size_and_ticks_expire_toasts() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        // Stamped five seconds ago, so the first real tick expires it.
        state.now -= Duration::from_secs(5);
        state.notify(crate::toast::Severity::Info, "hello");
        let (tx, rx) = unbounded_channel();
        let quitter = tx.clone();
        tokio::spawn(async move {
            // Leave time for at least one tick before quitting.
            tokio::time::sleep(TICK * 3).await;
            let _ = quitter.send(key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let state = run_app(&mut terminal, state, Vec::new(), tx, rx, &mut Headless)
            .await
            .unwrap();
        assert_eq!((state.area.width, state.area.height), (100, 30));
        assert_eq!(state.notifications.visible().len(), 0);
    }
}
