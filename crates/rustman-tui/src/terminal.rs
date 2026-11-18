//! Terminal setup and teardown. The guard and the panic hook both restore
//! cooked mode, the main screen and the cursor (spec: Launching the TUI).

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::thread::{self, ThreadId};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Raw mode on the alternate screen for as long as the guard lives.
pub struct TerminalGuard {
    terminal: CrosstermTerminal,
}

impl TerminalGuard {
    /// Install the panic hook, then switch the terminal to raw mode on the
    /// alternate screen with the cursor hidden. The mouse is not captured.
    pub fn enter() -> io::Result<Self> {
        Self::enter_with(false)
    }

    /// [`Self::enter`], capturing mouse events when `mouse` is true
    /// (`mouse = false` leaves them to the terminal's own selection).
    pub fn enter_with(mouse: bool) -> io::Result<Self> {
        install_panic_hook();
        set_owner(Some(thread::current().id()));
        enable_raw_mode()?;
        let terminal = setup(&mut io::stdout(), mouse)
            .inspect(|()| MOUSE_CAPTURED.store(mouse, Ordering::SeqCst))
            .and_then(|()| Terminal::new(CrosstermBackend::new(io::stdout())));
        match terminal {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                let _ = restore();
                set_owner(None);
                Err(err)
            }
        }
    }

    pub fn terminal(&mut self) -> &mut CrosstermTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore();
        set_owner(None);
    }
}

/// Hand the terminal to another program (the external editor): cooked
/// mode, the main screen, mouse released when `mouse`.
pub fn suspend(mouse: bool) -> io::Result<()> {
    let raw = disable_raw_mode();
    let screen = restore_screen(&mut io::stdout(), mouse);
    raw.and(screen)
}

/// Take the terminal back after [`suspend`].
pub fn resume(mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    setup(&mut io::stdout(), mouse)
}

/// Whether the live guard captured the mouse. On Windows releasing a
/// capture that was never enabled fails, so restore only releases it when set.
static MOUSE_CAPTURED: AtomicBool = AtomicBool::new(false);

/// The thread that holds the live guard: the one running the UI loop.
static OWNER: Mutex<Option<ThreadId>> = Mutex::new(None);

fn set_owner(owner: Option<ThreadId>) {
    if let Ok(mut current) = OWNER.lock() {
        *current = owner;
    }
}

/// True when a guard is live and this thread runs the UI loop. A panic on
/// another thread (an effect, the input reader) does not stop the loop by
/// itself; it is reported as an abort and the guard restores the terminal.
fn panic_ends_the_ui() -> bool {
    OWNER
        .lock()
        .is_ok_and(|owner| *owner == Some(thread::current().id()))
}

/// Switch `out` to the alternate screen with the cursor hidden, and enable
/// mouse reporting only when `mouse` is true.
pub fn setup(out: &mut impl Write, mouse: bool) -> io::Result<()> {
    execute!(out, EnterAlternateScreen, Hide)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    Ok(())
}

/// Put the terminal back: cooked mode, mouse released (when it was
/// captured), main screen, visible cursor. Safe to call more than once.
pub fn restore() -> io::Result<()> {
    let raw = disable_raw_mode();
    let mouse = MOUSE_CAPTURED.swap(false, Ordering::SeqCst);
    let screen = restore_screen(&mut io::stdout(), mouse);
    raw.and(screen)
}

/// Release the mouse when `mouse` is true, leave the alternate screen and
/// show the cursor. Every step runs even when an earlier one fails; the
/// first error is returned.
pub fn restore_screen(out: &mut impl Write, mouse: bool) -> io::Result<()> {
    let released = if mouse {
        execute!(out, DisableMouseCapture)
    } else {
        Ok(())
    };
    let left = execute!(out, LeaveAlternateScreen);
    let shown = execute!(out, Show);
    released.and(left).and(shown)
}

/// Restore the terminal before the previous hook prints the panic message,
/// so the message lands on the main screen in cooked mode. Only a panic on
/// the UI thread while a guard is live restores it.
fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if panic_ends_the_ui() {
                let _ = restore();
                set_owner(None);
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(mouse: bool) -> String {
        let mut out = Vec::new();
        setup(&mut out, mouse).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn only_the_ui_thread_panic_restores_while_a_guard_is_live() {
        assert!(!panic_ends_the_ui(), "no guard yet");
        set_owner(Some(thread::current().id()));
        assert!(panic_ends_the_ui());
        let other = thread::spawn(panic_ends_the_ui).join().unwrap();
        assert!(!other, "an effect or reader thread");
        set_owner(None);
        assert!(!panic_ends_the_ui(), "after the guard is dropped");
    }

    #[cfg(unix)]
    #[test]
    fn mouse_reporting_is_enabled_only_when_asked() {
        // Scenario "Mouse disabled": no mouse-tracking escape is written.
        let off = written(false);
        assert!(off.contains("\x1b[?1049h"), "{off:?}");
        assert!(!off.contains("\x1b[?1000h"), "{off:?}");
        assert!(written(true).contains("\x1b[?1000h"));
    }

    #[cfg(unix)]
    #[test]
    fn restore_releases_the_mouse_only_when_captured() {
        let mut out = Vec::new();
        restore_screen(&mut out, false).unwrap();
        let off = String::from_utf8(out).unwrap();
        assert!(
            off.contains("\x1b[?1049l") && off.contains("\x1b[?25h"),
            "{off:?}"
        );
        assert!(!off.contains("\x1b[?1000l"), "{off:?}");
        let mut out = Vec::new();
        restore_screen(&mut out, true).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("\x1b[?1000l"));
    }

    /// Fails the first write, then accepts everything.
    #[cfg(unix)]
    struct FailOnce {
        failed: bool,
        out: Vec<u8>,
    }

    #[cfg(unix)]
    impl Write for FailOnce {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(io::Error::other("first write fails"));
            }
            self.out.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_restore_step_does_not_skip_the_rest() {
        let mut out = FailOnce {
            failed: false,
            out: Vec::new(),
        };
        assert!(restore_screen(&mut out, true).is_err());
        let rest = String::from_utf8(out.out).unwrap();
        assert!(
            rest.contains("\x1b[?1049l") && rest.contains("\x1b[?25h"),
            "{rest:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn setup_writes_without_mouse() {
        // The Windows console API needs no escape; the call must still work.
        let _ = written(false);
    }

    #[cfg(windows)]
    #[test]
    fn restore_without_mouse_capture_does_not_release_it() {
        // Releasing a capture that was never enabled fails on Windows with
        // "Initial console modes not set" and used to skip the rest.
        let mut out = Vec::new();
        if let Err(err) = restore_screen(&mut out, false) {
            assert!(!err.to_string().contains("Initial console modes"), "{err}");
        }
    }
}
