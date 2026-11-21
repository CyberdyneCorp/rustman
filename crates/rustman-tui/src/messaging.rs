//! The live side of the messaging pane (add-messaging-client "TUI
//! messaging pane"): the messages a consume receives, as they arrive,
//! with a pause that freezes the list and a buffer of at most
//! [`BUFFER_CAP`] messages. When the buffer is full the consumer is
//! stopped, so it fetches nothing more, and what it collected is kept.
//! The buffer also holds at most [`BUFFER_BYTES`] of messages: beyond
//! that the oldest are dropped, and the pane says how many.
//!
//! Messages reach the TUI through the consume's feed, which redacts every
//! secret of the request before sending them.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::app::State;
use crate::toast::Severity;

/// The most messages the pane keeps for one consume.
pub const BUFFER_CAP: usize = 10_000;

/// The most bytes (of messages as JSON) the pane keeps for one consume;
/// the oldest messages are dropped beyond it.
pub const BUFFER_BYTES: usize = 64 * 1024 * 1024;

/// The messages of one consume, oldest first.
#[derive(Debug, Default)]
pub struct MessageLog {
    messages: VecDeque<(Value, usize)>,
    bytes: usize,
    dropped: usize,
}

impl MessageLog {
    /// Keep `message`, dropping the oldest messages while the log holds
    /// more than [`BUFFER_BYTES`]; false when the buffer is full and it
    /// was not kept.
    pub fn push(&mut self, message: Value) -> bool {
        if self.is_full() {
            return false;
        }
        let size = json_size(&message);
        self.bytes += size;
        self.messages.push_back((message, size));
        while self.bytes > BUFFER_BYTES && self.messages.len() > 1 {
            if let Some((_, size)) = self.messages.pop_front() {
                self.bytes -= size;
                self.dropped += 1;
            }
        }
        true
    }

    /// Whether the consume reached [`BUFFER_CAP`] messages, dropped ones
    /// included.
    pub fn is_full(&self) -> bool {
        self.received() >= BUFFER_CAP
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// The bytes the kept messages take.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// How many of the oldest messages were dropped to stay within
    /// [`BUFFER_BYTES`].
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Every message received, kept or dropped.
    pub fn received(&self) -> usize {
        self.dropped + self.messages.len()
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.messages.get(index).map(|(value, _)| value)
    }

    /// The messages from `first`, at most `count` of them.
    pub fn window(&self, first: usize, count: usize) -> Vec<Value> {
        self.messages
            .iter()
            .skip(first)
            .take(count)
            .map(|(value, _)| value.clone())
            .collect()
    }
}

/// The length of `value` as JSON.
fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

/// A consume's log, shared with the send that fills it.
pub type SharedMessages = Arc<Mutex<MessageLog>>;

/// The pane's view of its consumer: the log, whether a consume is
/// running, and the pause.
#[derive(Debug, Clone, Default)]
pub struct Consumer {
    log: SharedMessages,
    /// While paused: how many messages had been received.
    paused_at: Option<usize>,
    /// Stopped when the consume ends; `None` before the first one.
    running: Option<CancellationToken>,
}

impl Consumer {
    /// Start a new consume: an empty log, unpaused. The token is cancelled
    /// once the consume is over.
    pub fn connect(&mut self) -> (SharedMessages, CancellationToken) {
        self.log = SharedMessages::default();
        self.paused_at = None;
        let running = CancellationToken::new();
        self.running = Some(running.clone());
        (Arc::clone(&self.log), running)
    }

    pub fn is_running(&self) -> bool {
        self.running.as_ref().is_some_and(|r| !r.is_cancelled())
    }

    pub fn log(&self) -> MutexGuard<'_, MessageLog> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pause (freeze the list where it is) or resume.
    pub fn toggle_pause(&mut self) {
        self.paused_at = match self.paused_at {
            Some(_) => None,
            None => Some(self.log().received()),
        };
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// How many messages the list shows.
    pub fn shown(&self) -> usize {
        let log = self.log();
        let len = log.len();
        self.paused_at
            .map_or(len, |at| at.saturating_sub(log.dropped()).min(len))
    }
}

/// What a send needs to fill the pane: the log, and the token it cancels
/// when the consume is over.
#[derive(Debug, Clone)]
pub struct ConsumerLink {
    pub log: SharedMessages,
    pub running: CancellationToken,
}

/// Keep a consume's (redacted) messages for the pane. When the buffer is
/// full, `stop` ends the consume. Never returns: the runner holds the
/// feed's sender until the send is over.
pub async fn collect(
    mut messages: UnboundedReceiver<Value>,
    link: Option<&ConsumerLink>,
    stop: &CancellationToken,
) {
    while let Some(message) = messages.recv().await {
        let Some(link) = link else { continue };
        let mut log = link.log.lock().unwrap_or_else(PoisonError::into_inner);
        if !log.push(message) || log.is_full() {
            stop.cancel();
        }
    }
    std::future::pending::<()>().await;
}

/// `Esc` on a messaging tab whose consume runs: stop it, keeping what it
/// collected; the exchange is then shown. False when nothing runs or it
/// is already stopping, so the next `Esc` cancels the send outright.
pub fn stop(state: &mut State) -> bool {
    let Some(tab) = state.tabs.active() else {
        return false;
    };
    if tab.editor.messaging().is_none() {
        return false;
    }
    let Some(flight) = tab.in_flight.as_ref().filter(|f| !f.stop.is_cancelled()) else {
        return false;
    };
    flight.stop.cancel();
    state.notify(Severity::Info, "Stopping the consumer…");
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[test]
    fn the_buffer_holds_ten_thousand_messages() {
        let mut log = MessageLog::default();
        for n in 0..BUFFER_CAP {
            assert!(log.push(json!(n)));
        }
        assert!(log.is_full());
        assert!(!log.push(json!("one more")));
        assert_eq!(log.len(), BUFFER_CAP);
        assert_eq!(log.window(9_998, 5), [json!(9_998), json!(9_999)]);
    }

    #[test]
    fn the_oldest_messages_are_dropped_beyond_the_byte_bound() {
        // Regression: 10000 messages of up to 1 MiB each were all kept.
        let mut log = MessageLog::default();
        let big = json!("x".repeat(BUFFER_BYTES / 4));
        for _ in 0..6 {
            assert!(log.push(big.clone()));
        }
        assert!(log.bytes() <= BUFFER_BYTES, "{}", log.bytes());
        assert_eq!((log.len(), log.dropped(), log.received()), (3, 3, 6));
        let huge = json!("y".repeat(BUFFER_BYTES + 1));
        assert!(log.push(huge.clone()));
        assert_eq!(log.len(), 1, "the newest message is always kept");
        assert_eq!(log.get(0), Some(&huge));
        assert_eq!(log.dropped(), 6);
    }

    #[test]
    fn a_paused_list_shrinks_by_what_was_dropped() {
        let mut consumer = Consumer::default();
        let (log, _running) = consumer.connect();
        let big = json!("x".repeat(BUFFER_BYTES / 4));
        for _ in 0..3 {
            log.lock().unwrap().push(big.clone());
        }
        consumer.toggle_pause();
        for _ in 0..2 {
            log.lock().unwrap().push(big.clone());
        }
        assert_eq!(consumer.log().dropped(), 2);
        assert_eq!(consumer.shown(), 1, "only the third message is still kept");
    }

    #[test]
    fn a_paused_list_stays_where_it_was() {
        let mut consumer = Consumer::default();
        let (log, _running) = consumer.connect();
        log.lock().unwrap().push(json!(1));
        consumer.toggle_pause();
        log.lock().unwrap().push(json!(2));
        assert!(consumer.is_paused());
        assert_eq!(consumer.shown(), 1);
        consumer.toggle_pause();
        assert_eq!(consumer.shown(), 2);
    }

    #[tokio::test]
    async fn a_full_buffer_stops_the_consumer() {
        let mut consumer = Consumer::default();
        let (log, running) = consumer.connect();
        assert!(consumer.is_running());
        let link = ConsumerLink { log, running };
        let stop = CancellationToken::new();
        let (tx, rx) = unbounded_channel();
        for n in 0..=BUFFER_CAP {
            tx.send(json!(n)).unwrap();
        }
        drop(tx);
        let collecting = collect(rx, Some(&link), &stop);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), collecting).await;
        assert!(stop.is_cancelled());
        assert_eq!(consumer.log().len(), BUFFER_CAP);
    }
}
