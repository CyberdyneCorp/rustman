//! The live side of the realtime pane (add-realtime-client "Message
//! timeline"): the session's timeline as it arrives, its filters, the
//! frames typed in the Compose tab, and closing the session with `Esc`.
//!
//! Entries reach the TUI through the session's feed, which redacts every
//! secret of the request before sending them, as for JSON-RPC
//! subscriptions. The log keeps at most `limits.max_timeline` entries and
//! the view draws only the rows that fit, so a long session never blocks
//! input.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use base64::Engine;
use rustman_core::vars::{ResolvedBinary, ResolvedFrame};
use rustman_realtime::{Command, Direction, FeedItem, FeedReceiver, Timeline, TimelineEntry};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::app::State;
use crate::toast::Severity;

/// The type of the frame the Compose tab sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeMode {
    Text,
    Json,
    /// Base64-encoded bytes.
    Binary,
}

impl ComposeMode {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Text => Self::Json,
            Self::Json => Self::Binary,
            Self::Binary => Self::Text,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "JSON",
            Self::Binary => "binary (base64)",
        }
    }

    /// The frame `text` makes, or why it cannot be sent.
    pub fn frame(self, text: &str) -> Result<ResolvedFrame, String> {
        match self {
            Self::Text => Ok(ResolvedFrame::Text(text.to_owned())),
            Self::Json => serde_json::from_str(text)
                .map(ResolvedFrame::Json)
                .map_err(|e| format!("not JSON: {e}")),
            Self::Binary => {
                let data = text.split_whitespace().collect::<String>();
                base64::engine::general_purpose::STANDARD
                    .decode(&data)
                    .map(|_| ResolvedFrame::Binary(ResolvedBinary::Base64(data)))
                    .map_err(|e| format!("not base64: {e}"))
            }
        }
    }
}

/// Which entries the Timeline tab shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineFilter {
    pub dir: Option<Direction>,
    /// The frame or event type.
    pub kind: Option<String>,
    /// Text the payload contains.
    pub text: String,
}

impl TimelineFilter {
    /// All, sent, received, system, all...
    pub fn cycle_dir(&mut self) {
        self.dir = match self.dir {
            None => Some(Direction::Sent),
            Some(Direction::Sent) => Some(Direction::Received),
            Some(Direction::Received) => Some(Direction::System),
            Some(Direction::System) => None,
        };
    }

    /// All, then each type of `kinds` in turn.
    pub fn cycle_kind(&mut self, kinds: &[String]) {
        let at = self
            .kind
            .as_ref()
            .and_then(|k| kinds.iter().position(|seen| seen == k));
        self.kind = match at {
            None => kinds.first().cloned(),
            Some(i) => kinds.get(i + 1).cloned(),
        };
    }

    pub fn matches(&self, entry: &TimelineEntry) -> bool {
        self.dir.is_none_or(|dir| entry.dir == dir)
            && self.kind.as_ref().is_none_or(|kind| entry.kind == *kind)
            && (self.text.is_empty() || entry.payload.contains(&self.text))
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The entries of a session, capped like its timeline (entries and
/// payload bytes).
#[derive(Debug)]
pub struct RealtimeLog(Timeline);

impl RealtimeLog {
    pub fn new(max: u32) -> Self {
        Self(Timeline::new(Instant::now(), max))
    }

    /// Append `entry`, dropping the oldest while the log is full or would
    /// hold too many payload bytes.
    pub fn push(&mut self, entry: TimelineEntry) {
        self.0.push(entry);
    }

    /// Note, in place, that the feed skipped `n` entries because the pane
    /// fell behind; the exchange shown once the session ends has them.
    pub fn skipped(&mut self, n: u64) {
        let t_ms = self
            .len()
            .checked_sub(1)
            .and_then(|i| self.get(i))
            .map_or(0, |e| e.t_ms);
        let what = if n == 1 { "entry" } else { "entries" };
        self.push(TimelineEntry {
            dir: Direction::System,
            t_ms,
            size: 0,
            kind: "skipped".to_owned(),
            payload: format!("{n} {what} not shown: the display fell behind"),
            id: None,
            rtt_ms: None,
        });
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.0.dropped()
    }

    pub fn get(&self, index: usize) -> Option<&TimelineEntry> {
        self.0.get(index)
    }

    fn iter(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.0.entries()
    }
}

/// A session's log, shared with the send that fills it.
pub type SharedLog = Arc<Mutex<RealtimeLog>>;

/// What a send needs to feed the pane and take its frames.
#[derive(Debug, Clone)]
pub struct LiveLink {
    pub log: SharedLog,
    commands: Arc<Mutex<Option<Receiver<Command>>>>,
}

impl LiveLink {
    /// The frames and closes the pane sends; `None` once taken.
    pub fn take_commands(&self) -> Option<Receiver<Command>> {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// The pane's view of its session: the log and the way to send frames.
#[derive(Debug, Clone)]
pub struct Live {
    log: SharedLog,
    outbox: Option<Sender<Command>>,
    max: u32,
}

impl Live {
    pub fn new(max: u32) -> Self {
        Self {
            log: Arc::new(Mutex::new(RealtimeLog::new(max))),
            outbox: None,
            max,
        }
    }

    /// Start a new session: an empty log and a fresh command channel.
    pub fn connect(&mut self) -> LiveLink {
        self.log = Arc::new(Mutex::new(RealtimeLog::new(self.max)));
        let (tx, rx) = rustman_realtime::command_channel();
        self.outbox = Some(tx);
        LiveLink {
            log: Arc::clone(&self.log),
            commands: Arc::new(Mutex::new(Some(rx))),
        }
    }

    /// Whether a session is running (its command channel is open).
    pub fn is_connected(&self) -> bool {
        self.outbox.as_ref().is_some_and(|tx| !tx.is_closed())
    }

    /// Send `command` to the running session; false when there is none,
    /// or its (bounded) queue is full.
    pub fn send(&self, command: Command) -> bool {
        self.outbox
            .as_ref()
            .is_some_and(|tx| tx.try_send(command).is_ok())
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, RealtimeLog> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The positions of the entries `filter` shows, oldest first.
    pub fn matching(&self, filter: &TimelineFilter) -> Vec<usize> {
        let log = self.log();
        if filter.is_empty() {
            return (0..log.len()).collect();
        }
        log.iter()
            .enumerate()
            .filter(|(_, e)| filter.matches(e))
            .map(|(i, _)| i)
            .collect()
    }

    /// The types seen so far, sorted.
    pub fn kinds(&self) -> Vec<String> {
        let log = self.log();
        let kinds: BTreeSet<&str> = log.iter().map(|e| e.kind.as_str()).collect();
        kinds.into_iter().map(str::to_owned).collect()
    }

    /// The entries at `positions`.
    pub fn entries(&self, positions: &[usize]) -> Vec<TimelineEntry> {
        let log = self.log();
        positions
            .iter()
            .filter_map(|i| log.get(*i).cloned())
            .collect()
    }
}

/// Keep a session's (redacted) entries for the pane, with a note where
/// the bounded feed skipped some. Never returns: the runner holds the
/// feed's sender until the send is over.
pub async fn collect(mut entries: FeedReceiver, log: Option<&SharedLog>) {
    while let Some(item) = entries.recv().await {
        if let Some(log) = log {
            let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
            match item {
                FeedItem::Entry(entry) => log.push(entry),
                FeedItem::Skipped(n) => log.skipped(n),
            }
        }
    }
    std::future::pending::<()>().await;
}

/// Whether the active tab is a realtime one with a session in flight.
pub fn is_connected(state: &State) -> bool {
    state.tabs.active().is_some_and(|tab| {
        tab.in_flight.is_some() && tab.editor.realtime().is_some_and(|e| e.live.is_connected())
    })
}

/// `request.send` while connected: send the Compose tab's frame.
pub fn send_frame(state: &mut State) {
    let Some(editor) = state.tabs.active().and_then(|t| t.editor.realtime()) else {
        return;
    };
    let text = editor.compose.text();
    let (severity, message) = match editor.mode.frame(&text) {
        Ok(_) if text.is_empty() => (
            Severity::Info,
            "Type a frame in the Compose tab to send it".to_owned(),
        ),
        Ok(frame) => {
            let delivered = editor.live.send(Command::Send(frame));
            sent(delivered, editor.live.is_connected(), editor.mode)
        }
        Err(why) => (Severity::Error, format!("Cannot send: {why}")),
    };
    state.notify(severity, message);
}

/// The notice after sending a frame of `mode`.
fn sent(delivered: bool, connected: bool, mode: ComposeMode) -> (Severity, String) {
    match (delivered, connected) {
        (true, _) => (Severity::Info, format!("Sent a {} frame", mode.label())),
        (false, true) => (
            Severity::Warn,
            "The session is busy sending earlier frames; try again".to_owned(),
        ),
        (false, false) => (Severity::Warn, "The session is closed".to_owned()),
    }
}

/// `Esc` on a realtime tab whose session is open: close it (code 1000),
/// keeping its timeline; the exchange is then shown. False when there is
/// no open session, or it is already closing, so the next `Esc` cancels
/// the send outright.
pub fn close(state: &mut State) -> bool {
    let Some(tab) = state.tabs.active() else {
        return false;
    };
    if tab.editor.realtime().is_none() {
        return false;
    }
    let Some(flight) = tab.in_flight.as_ref().filter(|f| !f.stop.is_cancelled()) else {
        return false;
    };
    flight.stop.cancel();
    state.notify(Severity::Info, "Closing the session…");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dir: Direction, kind: &str, payload: &str) -> TimelineEntry {
        TimelineEntry {
            dir,
            t_ms: 1,
            size: payload.len() as u64,
            kind: kind.to_owned(),
            payload: payload.to_owned(),
            id: None,
            rtt_ms: None,
        }
    }

    #[test]
    fn filters_combine_direction_type_and_text() {
        // Scenario "Filter received JSON".
        let live = Live::new(1000);
        {
            let mut log = live.log();
            for n in 0..20 {
                log.push(entry(
                    Direction::Sent,
                    "json",
                    &format!("{{\"op\":\"trade\",\"n\":{n}}}"),
                ));
            }
            for n in 0..80 {
                let what = if n % 2 == 0 { "trade" } else { "quote" };
                log.push(entry(
                    Direction::Received,
                    "json",
                    &format!("{{\"{what}\":{n}}}"),
                ));
            }
        }
        let mut filter = TimelineFilter::default();
        filter.cycle_dir();
        filter.cycle_dir();
        assert_eq!(filter.dir, Some(Direction::Received));
        filter.text = "trade".into();
        let shown = live.entries(&live.matching(&filter));
        assert_eq!(shown.len(), 40);
        assert!(
            shown
                .iter()
                .all(|e| e.dir == Direction::Received && e.payload.contains("trade"))
        );
        filter.cycle_kind(&live.kinds());
        assert_eq!(filter.kind.as_deref(), Some("json"));
        filter.cycle_kind(&live.kinds());
        assert_eq!(filter.kind, None);
    }

    #[test]
    fn the_log_keeps_the_newest_entries_and_counts_the_dropped() {
        let mut log = RealtimeLog::new(3);
        for n in 0..5 {
            log.push(entry(Direction::Received, "text", &n.to_string()));
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.dropped(), 2);
        assert_eq!(log.get(0).unwrap().payload, "2");
    }

    #[test]
    fn composed_frames_are_checked_before_they_are_sent() {
        assert_eq!(
            ComposeMode::Text.frame("hi").unwrap(),
            ResolvedFrame::Text("hi".into())
        );
        assert_eq!(
            ComposeMode::Json.frame("{\"a\": 1}").unwrap(),
            ResolvedFrame::Json(serde_json::json!({"a": 1}))
        );
        assert!(
            ComposeMode::Json
                .frame("{")
                .unwrap_err()
                .starts_with("not JSON")
        );
        assert!(ComposeMode::Binary.frame("aGk=").is_ok());
        assert!(ComposeMode::Binary.frame("***").is_err());
        assert_eq!(ComposeMode::Binary.next(), ComposeMode::Text);
    }

    #[test]
    fn a_connected_pane_sends_frames_until_the_session_ends() {
        let mut live = Live::new(10);
        assert!(!live.is_connected());
        let link = live.connect();
        assert!(live.is_connected());
        let mut rx = link.take_commands().unwrap();
        assert!(link.take_commands().is_none(), "taken once");
        assert!(live.send(Command::Send(ResolvedFrame::Text("a".into()))));
        assert!(matches!(rx.try_recv(), Ok(Command::Send(_))));
        drop(rx);
        assert!(!live.is_connected());
        assert!(!live.send(Command::Send(ResolvedFrame::Text("b".into()))));
    }

    /// A pane slower than its session gets a note where the bounded feed
    /// skipped entries (#81).
    #[tokio::test]
    async fn skipped_entries_show_as_a_note_in_place() {
        let (tx, rx) = rustman_realtime::feed_channel_with(2, usize::MAX);
        for p in ["a", "b", "c", "d"] {
            tx.send(entry(Direction::Received, "text", p));
        }
        drop(tx);
        let live = Live::new(1000);
        let log = Arc::clone(&live.log);
        let collected = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            collect(rx, Some(&log)),
        );
        assert!(collected.await.is_err(), "collect never returns");
        let log = live.log();
        let shown: Vec<&str> = (0..log.len())
            .map(|i| log.get(i).unwrap().payload.as_str())
            .collect();
        assert_eq!(
            shown,
            ["a", "b", "2 entries not shown: the display fell behind"]
        );
        assert_eq!(log.get(2).unwrap().dir, Direction::System);
    }
}
