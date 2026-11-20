//! The terminal of a live load dashboard (`rustman bench` and `rustman
//! stress`): the alternate screen of stdout with the cursor hidden, left
//! (and the cursor shown again) when dropped, even on a panic.

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

pub(crate) struct LiveScreen {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl LiveScreen {
    /// The alternate screen; `None` (with the screen left again) when the
    /// terminal cannot be used.
    pub(crate) fn open() -> Option<Self> {
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide).ok()?;
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Some(Self { terminal }),
            Err(_) => {
                let _ = execute!(std::io::stdout(), LeaveAlternateScreen, Show);
                None
            }
        }
    }

    /// Draw one frame; a failed draw is skipped.
    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) {
        let _ = self.terminal.draw(render);
    }
}

impl Drop for LiveScreen {
    fn drop(&mut self) {
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen, Show);
    }
}
