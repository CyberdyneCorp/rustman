//! Toasts and the notification log (spec: Toasts).
//!
//! Time comes in from outside (`now` arguments, fed by `Action::Tick`), so
//! the expiry rules are tested with a mocked clock.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use crate::theme::Token;

/// How many toasts are on screen at once.
pub const MAX_VISIBLE: usize = 3;
/// How many entries the notification log keeps.
pub const LOG_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warn,
    Error,
}

impl Severity {
    /// How long a toast of this severity stays up when not dismissed.
    pub fn timeout(self) -> Duration {
        match self {
            Self::Info | Self::Success => Duration::from_secs(3),
            Self::Warn | Self::Error => Duration::from_secs(10),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warn => "warning",
            Self::Error => "error",
        }
    }

    pub fn token(self) -> Token {
        match self {
            Self::Info => Token::Info,
            Self::Success => Token::Ok,
            Self::Warn => Token::Warn,
            Self::Error => Token::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub severity: Severity,
    pub message: String,
    pub expires: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub severity: Severity,
    pub message: String,
}

/// The toast stack and the log of everything that was toasted.
#[derive(Debug, Clone, Default)]
pub struct Notifications {
    /// Oldest first.
    stack: VecDeque<Toast>,
    /// Oldest first.
    log: VecDeque<LogEntry>,
}

impl Notifications {
    /// Show a toast and log it. The oldest toast leaves the screen when a
    /// fourth arrives; it stays in the log.
    pub fn push(&mut self, now: Instant, severity: Severity, message: impl Into<String>) {
        let message = message.into();
        self.log.push_back(LogEntry {
            severity,
            message: message.clone(),
        });
        if self.log.len() > LOG_LIMIT {
            self.log.pop_front();
        }
        self.stack.push_back(Toast {
            severity,
            message,
            expires: now + severity.timeout(),
        });
        if self.stack.len() > MAX_VISIBLE {
            self.stack.pop_front();
        }
    }

    /// Drop the toasts whose time is up. True if any was dropped.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.stack.len();
        self.stack.retain(|t| t.expires > now);
        self.stack.len() != before
    }

    /// Dismiss the newest toast. True if there was one.
    pub fn dismiss_newest(&mut self) -> bool {
        self.stack.pop_back().is_some()
    }

    /// Dismiss the toast at `index` in [`Self::visible`] order.
    pub fn dismiss(&mut self, index: usize) {
        if let Some(i) = self.stack.len().checked_sub(index + 1) {
            self.stack.remove(i);
        }
    }

    /// The toasts on screen, newest first.
    pub fn visible(&self) -> impl ExactSizeIterator<Item = &Toast> {
        self.stack.iter().rev()
    }

    /// Everything logged, newest first.
    pub fn log(&self) -> impl ExactSizeIterator<Item = &LogEntry> {
        self.log.iter().rev()
    }
}

/// Toast width limits, in columns.
const MIN_WIDTH: u16 = 24;
const MAX_WIDTH: u16 = 50;
/// Message lines shown per toast before it is cut (the log has it all).
const MAX_LINES: u16 = 3;

/// Where each toast is drawn: the top-right corner of `area`, newest on
/// top, sized to the message. Toasts that do not fit are left out.
pub fn toast_rects<'a>(area: Rect, toasts: impl Iterator<Item = &'a Toast>) -> Vec<Rect> {
    let mut rects = Vec::new();
    let mut y = area.y;
    // Keep the status bar row free.
    let bottom = area.bottom().saturating_sub(1);
    for toast in toasts {
        let chars = u16::try_from(toast.message.chars().count()).unwrap_or(u16::MAX);
        let width = chars
            .saturating_add(4)
            .clamp(MIN_WIDTH, MAX_WIDTH)
            .min(area.width);
        let inner = width.saturating_sub(4).max(1);
        let lines = chars.div_ceil(inner).clamp(1, MAX_LINES);
        let height = lines + 2;
        if y + height > bottom {
            break;
        }
        rects.push(Rect::new(area.right() - width, y, width, height));
        y += height;
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(n: &Notifications) -> Vec<&str> {
        n.visible().map(|t| t.message.as_str()).collect()
    }

    #[test]
    fn info_and_success_leave_after_three_seconds() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        n.push(t0, Severity::Info, "copied");
        n.push(t0, Severity::Success, "saved");
        assert!(!n.expire(t0 + Duration::from_millis(2999)));
        assert_eq!(n.visible().len(), 2);
        assert!(n.expire(t0 + Duration::from_secs(3)));
        assert_eq!(n.visible().len(), 0);
    }

    #[test]
    fn errors_and_warnings_stay_for_ten_seconds() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        n.push(t0, Severity::Error, "Permission denied (os error 13)");
        n.push(t0, Severity::Warn, "careful");
        n.expire(t0 + Duration::from_secs(9));
        assert_eq!(messages(&n), ["careful", "Permission denied (os error 13)"]);
        n.expire(t0 + Duration::from_secs(10));
        assert!(messages(&n).is_empty());
        let logged: Vec<_> = n.log().map(|e| e.message.as_str()).collect();
        assert_eq!(logged, ["careful", "Permission denied (os error 13)"]);
    }

    #[test]
    fn at_most_three_are_stacked_but_all_are_logged() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        for message in ["one", "two", "three", "four"] {
            n.push(t0, Severity::Warn, message);
        }
        assert_eq!(messages(&n), ["four", "three", "two"]);
        assert_eq!(n.log().len(), 4);
    }

    #[test]
    fn dismissing_newest_or_by_position() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        for message in ["a", "b", "c"] {
            n.push(t0, Severity::Error, message);
        }
        assert!(n.dismiss_newest());
        assert_eq!(messages(&n), ["b", "a"]);
        n.dismiss(1);
        assert_eq!(messages(&n), ["b"]);
        n.dismiss(5);
        assert_eq!(messages(&n), ["b"]);
        assert!(n.dismiss_newest());
        assert!(!n.dismiss_newest());
        assert_eq!(n.log().len(), 3);
    }

    #[test]
    fn toasts_stack_in_the_top_right_corner() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        n.push(t0, Severity::Info, "short");
        n.push(t0, Severity::Error, "x".repeat(100));
        let rects = toast_rects(Rect::new(0, 0, 160, 45), n.visible());
        assert_eq!(rects[0], Rect::new(110, 0, 50, 5), "long: 3 lines, cut");
        assert_eq!(rects[1], Rect::new(136, 5, 24, 3));
        assert!(toast_rects(Rect::new(0, 0, 30, 4), n.visible()).is_empty());
        assert!(toast_rects(Rect::new(0, 0, 0, 0), n.visible()).is_empty());
    }

    #[test]
    fn the_log_is_bounded() {
        let t0 = Instant::now();
        let mut n = Notifications::default();
        for i in 0..=LOG_LIMIT {
            n.push(t0, Severity::Info, i.to_string());
        }
        assert_eq!(n.log().len(), LOG_LIMIT);
        assert_eq!(n.log().last().unwrap().message, "1");
    }
}
