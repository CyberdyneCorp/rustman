//! How a session runs (design D3): headless with bounds, or interactive
//! until the user closes it, with an optional live feed of redacted
//! timeline entries and a command channel for frames typed while it runs.

use std::time::{Duration, Instant};

use rustman_core::Redactor;
use rustman_core::model::{CloseStep, RealtimeLimits};
use rustman_core::vars::ResolvedFrame;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::timeline::TimelineEntry;

/// When a headless session stops listening, once its steps are done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Received messages (counted over the whole session).
    pub max_messages: u64,
    /// Time since the session started.
    pub duration: Duration,
}

impl Bounds {
    /// The bounds of `limits`, with their defaults.
    pub fn of(limits: &RealtimeLimits) -> Self {
        Self {
            max_messages: u64::from(limits.effective_max_messages()),
            duration: Duration::from_millis(limits.effective_duration_ms()),
        }
    }
}

/// Something to do on an open session, from outside it.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Send(ResolvedFrame),
    Close(CloseStep),
}

/// How many commands may wait for a session: a sender faster than the
/// session (stdin piped from a large file) waits instead of queueing
/// without bound.
pub const COMMAND_QUEUE: usize = 64;

/// The bounded channel commands reach a session through.
pub fn command_channel() -> (mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    mpsc::channel(COMMAND_QUEUE)
}

/// How one session runs.
#[derive(Debug, Default)]
pub struct SessionOptions {
    /// `None`: interactive, open until closed or cancelled.
    pub bounds: Option<Bounds>,
    /// Closes the session (Ctrl+C, the TUI's close control).
    pub cancel: CancellationToken,
    /// Gets every timeline entry as it is recorded, redacted.
    pub feed: Option<mpsc::UnboundedSender<TimelineEntry>>,
    /// Frames and closes to perform while the session runs.
    pub commands: Option<mpsc::Receiver<Command>>,
}

impl SessionOptions {
    /// A headless session that stops at `bounds` or on `cancel`.
    pub fn headless(bounds: Bounds, cancel: CancellationToken) -> Self {
        Self {
            bounds: Some(bounds),
            cancel,
            ..Self::default()
        }
    }
}

/// Hand `entry`, redacted, to whoever follows the session.
pub(crate) fn forward(
    feed: Option<&mpsc::UnboundedSender<TimelineEntry>>,
    redactor: &Redactor,
    entry: TimelineEntry,
) {
    if let Some(feed) = feed {
        let _ = feed.send(entry.redacted(redactor));
    }
}

/// Wait until `deadline`; forever when there is none.
pub(crate) async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// The next command; forever when there is no command channel.
pub(crate) async fn recv(commands: &mut Option<mpsc::Receiver<Command>>) -> Option<Command> {
    match commands {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}
