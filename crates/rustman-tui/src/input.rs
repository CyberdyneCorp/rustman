//! The terminal input reader: a thread that turns crossterm events (keys,
//! mouse, resizes) into actions on the action channel, so the loop never
//! blocks on the terminal.

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{self, Event, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::app::{Abort, Action};
use crate::effects::panic_text;

/// How long one poll waits before checking for shutdown.
const POLL: Duration = Duration::from_millis(50);

/// Pauses the reader while another program (the external editor) owns the
/// terminal. The reader holds the lock while it polls, so pausing waits for
/// the poll in progress to end.
#[derive(Debug, Clone, Default)]
pub struct Gate(Arc<Mutex<bool>>);

impl Gate {
    /// Stop reading (returns once no read is in progress) or resume.
    pub fn set_paused(&self, paused: bool) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = paused;
    }

    /// Run `read` unless paused; while paused, wait one poll period.
    fn read(
        &self,
        read: impl FnOnce() -> io::Result<Option<Action>>,
    ) -> io::Result<Option<Action>> {
        let paused = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if *paused {
            drop(paused);
            std::thread::sleep(POLL);
            return Ok(None);
        }
        read()
    }
}

/// Read terminal events until `stop` is cancelled or the loop is gone. If
/// reading fails or panics the loop is aborted, since it could no longer be
/// quit from the keyboard.
pub fn spawn_reader(
    actions: UnboundedSender<Action>,
    stop: CancellationToken,
    gate: Gate,
) -> JoinHandle<()> {
    std::thread::spawn(move || read_until_stopped(&actions, &stop, || gate.read(next_action)))
}

fn read_until_stopped(
    actions: &UnboundedSender<Action>,
    stop: &CancellationToken,
    mut next: impl FnMut() -> io::Result<Option<Action>>,
) {
    let read = std::panic::catch_unwind(AssertUnwindSafe(|| {
        while !stop.is_cancelled() && !actions.is_closed() {
            match next() {
                Ok(Some(action)) => {
                    if actions.send(action).is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => {}
                Err(err) => return Err(format!("terminal input failed: {err}")),
            }
        }
        Ok(())
    }));
    let failure = match read {
        Ok(result) => result.err(),
        Err(payload) => Some(panic_text(&*payload)),
    };
    if let Some(message) = failure {
        let _ = actions.send(Action::Abort(Abort::Failed(message)));
    }
}

fn next_action() -> io::Result<Option<Action>> {
    if !event::poll(POLL)? {
        return Ok(None);
    }
    Ok(to_action(event::read()?))
}

/// The action for a terminal event, if it is one the UI handles. Pointer
/// moves without a button are dropped: nothing uses hover, and they would
/// flood the (unbounded) action channel.
pub fn to_action(event: Event) -> Option<Action> {
    match event {
        Event::Key(key) => Some(Action::Key(key)),
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => None,
        Event::Mouse(mouse) => Some(Action::Mouse(mouse)),
        Event::Resize(width, height) => Some(Action::Resize { width, height }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};

    use tokio::sync::mpsc::unbounded_channel;

    fn failure(next: impl FnMut() -> io::Result<Option<Action>>) -> String {
        let (tx, mut rx) = unbounded_channel();
        read_until_stopped(&tx, &CancellationToken::new(), next);
        let Ok(Action::Abort(Abort::Failed(message))) = rx.try_recv() else {
            panic!("expected an abort");
        };
        message
    }

    #[test]
    fn a_failed_or_panicking_reader_aborts_the_loop() {
        let read_error = failure(|| Err(io::Error::other("tty gone")));
        assert!(read_error.contains("tty gone"), "{read_error}");
        let panicked = failure(|| panic!("reader bug"));
        assert!(panicked.contains("reader bug"), "{panicked}");
    }

    #[test]
    fn a_paused_gate_does_not_read() {
        let gate = Gate::default();
        gate.set_paused(true);
        let read = gate.read(|| panic!("read while paused"));
        assert!(matches!(read, Ok(None)));
        gate.set_paused(false);
        assert!(matches!(
            gate.read(|| Ok(Some(Action::Tick(std::time::Instant::now())))),
            Ok(Some(_))
        ));
    }

    #[test]
    fn a_stopped_reader_sends_nothing() {
        let (tx, mut rx) = unbounded_channel();
        let stop = CancellationToken::new();
        stop.cancel();
        read_until_stopped(&tx, &stop, || Ok(None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn keys_and_resizes_become_actions() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(to_action(Event::Key(key)), Some(Action::Key(k)) if k == key));
        assert!(matches!(
            to_action(Event::Resize(80, 24)),
            Some(Action::Resize {
                width: 80,
                height: 24
            })
        ));
        assert!(to_action(Event::FocusGained).is_none());
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(to_action(Event::Mouse(mouse)), Some(Action::Mouse(m)) if m == mouse));
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..mouse
        };
        assert!(to_action(Event::Mouse(moved)).is_none());
    }
}
