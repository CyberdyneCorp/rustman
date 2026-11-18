//! The application loop: draw, wait for the next action, update, dispatch
//! effects (design D2). Nothing here awaits network or disk I/O.

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app::{Action, Effect, State, update};
use crate::effects;
use crate::view::view;

/// Run until the state asks to quit. `tx` is the sending side of the single
/// action channel; the effect runner reports completions on it.
pub async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut state: State,
    initial: Vec<Effect>,
    tx: UnboundedSender<Action>,
    mut rx: UnboundedReceiver<Action>,
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
    let runner = effects::spawn_runner(tx);
    dispatch(&runner, initial);
    while !state.quit {
        terminal.draw(|frame| view(&state, frame))?;
        let Some(action) = rx.recv().await else {
            break;
        };
        dispatch(&runner, update(&mut state, action));
        // Apply everything already queued before drawing again.
        while !state.quit {
            let Ok(action) = rx.try_recv() else { break };
            dispatch(&runner, update(&mut state, action));
        }
    }
    Ok(state)
}

/// How often the clock ticks (toast expiry, later spinners).
const TICK: Duration = Duration::from_millis(250);

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

fn dispatch(runner: &UnboundedSender<Effect>, effects: Vec<Effect>) {
    for effect in effects {
        // The runner lives as long as this loop, so a send cannot fail here.
        let _ = runner.send(effect);
    }
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
        let state = run_app(&mut terminal, state, effects, tx, rx)
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
        run_app(&mut terminal, state, effects, tx, rx)
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
        let state = run_app(&mut terminal, state, Vec::new(), tx, rx)
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
        let state = run_app(&mut terminal, state, Vec::new(), tx, rx)
            .await
            .unwrap();
        assert_eq!((state.area.width, state.area.height), (100, 30));
        assert_eq!(state.notifications.visible().len(), 0);
    }
}
