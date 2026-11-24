//! The live feed of a session: redacted timeline entries for whoever
//! follows it (the CLI, the TUI), through a bounded channel.
//!
//! A consumer slower than the session never slows it down or makes the
//! feed grow: once [`FEED_QUEUE`] entries or [`FEED_MAX_BYTES`] of them
//! wait, further entries are not shown, only counted, and the consumer
//! gets one [`FeedItem::Skipped`] marker with the count before the next
//! entry that fits (or when the feed ends). The session's timeline and
//! its record are filled as before, so they stay complete.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use crate::timeline::TimelineEntry;

/// The most entries that wait for a consumer.
pub const FEED_QUEUE: usize = 1024;

/// The most bytes of entries ([`TimelineEntry::held_bytes`]) that wait
/// for a consumer (64 MiB). An entry larger than that is still shown when
/// nothing else waits.
pub const FEED_MAX_BYTES: usize = 64 * 1024 * 1024;

/// What a feed delivers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedItem {
    Entry(TimelineEntry),
    /// This many entries were not shown because the consumer fell behind;
    /// the session's timeline still holds them.
    Skipped(u64),
}

/// What the two ends of a feed share.
#[derive(Debug)]
struct Shared {
    /// Entries not shown and not yet reported.
    skipped: AtomicU64,
    /// The bytes of the entries waiting.
    bytes: AtomicUsize,
    max_bytes: usize,
}

/// The sending end of a feed; clones feed the same consumer.
#[derive(Clone, Debug)]
pub struct FeedSender {
    tx: mpsc::Sender<FeedItem>,
    shared: Arc<Shared>,
}

/// The receiving end of a feed.
#[derive(Debug)]
pub struct FeedReceiver {
    rx: mpsc::Receiver<FeedItem>,
    shared: Arc<Shared>,
}

/// A feed holding at most [`FEED_QUEUE`] entries and [`FEED_MAX_BYTES`].
pub fn feed_channel() -> (FeedSender, FeedReceiver) {
    feed_channel_with(FEED_QUEUE, FEED_MAX_BYTES)
}

/// A feed holding at most `entries` entries (at least 1) and `max_bytes`.
pub fn feed_channel_with(entries: usize, max_bytes: usize) -> (FeedSender, FeedReceiver) {
    let (tx, rx) = mpsc::channel(entries.max(1));
    let shared = Arc::new(Shared {
        skipped: AtomicU64::new(0),
        bytes: AtomicUsize::new(0),
        max_bytes,
    });
    let sender = FeedSender {
        tx,
        shared: Arc::clone(&shared),
    };
    (sender, FeedReceiver { rx, shared })
}

impl FeedSender {
    /// Offer `entry` to the consumer, never waiting: when the feed is
    /// full, it is counted as skipped instead.
    pub fn send(&self, entry: TimelineEntry) {
        self.send_with(|| entry);
    }

    /// [`FeedSender::send`] the entry `make` builds, building it only when
    /// there is room for it.
    pub fn send_with(&self, make: impl FnOnce() -> TimelineEntry) {
        if self.tx.is_closed() {
            return;
        }
        if self.shared.skipped.load(Ordering::Acquire) > 0 && !self.report_skipped() {
            self.skip();
            return;
        }
        let Ok(permit) = self.tx.try_reserve() else {
            self.skip();
            return;
        };
        let entry = make();
        let size = entry.held_bytes();
        let waiting = self.shared.bytes.load(Ordering::Acquire);
        if waiting > 0 && waiting.saturating_add(size) > self.shared.max_bytes {
            drop(permit);
            self.skip();
            return;
        }
        self.shared.bytes.fetch_add(size, Ordering::AcqRel);
        permit.send(FeedItem::Entry(entry));
    }

    /// Queue the marker of the entries skipped so far; false when there is
    /// no room for it.
    fn report_skipped(&self) -> bool {
        let Ok(permit) = self.tx.try_reserve() else {
            return false;
        };
        let skipped = self.shared.skipped.swap(0, Ordering::AcqRel);
        if skipped > 0 {
            permit.send(FeedItem::Skipped(skipped));
        }
        true
    }

    fn skip(&self) {
        self.shared.skipped.fetch_add(1, Ordering::AcqRel);
    }
}

impl FeedReceiver {
    /// The next item; `None` once every sender is gone and everything,
    /// including the last marker, was received.
    pub async fn recv(&mut self) -> Option<FeedItem> {
        if self.rx.is_empty()
            && let Some(marker) = self.take_skipped()
        {
            return Some(marker);
        }
        match self.rx.recv().await {
            Some(item) => Some(self.received(item)),
            None => self.take_skipped(),
        }
    }

    /// The next item without waiting; `None` when nothing waits.
    pub fn try_recv(&mut self) -> Option<FeedItem> {
        match self.rx.try_recv() {
            Ok(item) => Some(self.received(item)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => self.take_skipped(),
        }
    }

    /// How many entries wait.
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }

    /// The bytes of the entries waiting.
    pub fn waiting_bytes(&self) -> usize {
        self.shared.bytes.load(Ordering::Acquire)
    }

    fn received(&self, item: FeedItem) -> FeedItem {
        if let FeedItem::Entry(entry) = &item {
            self.shared
                .bytes
                .fetch_sub(entry.held_bytes(), Ordering::AcqRel);
        }
        item
    }

    /// The marker of the entries skipped since the last one, if any. Only
    /// called with nothing waiting, so it comes after every entry queued
    /// before them.
    fn take_skipped(&self) -> Option<FeedItem> {
        let skipped = self.shared.skipped.swap(0, Ordering::AcqRel);
        (skipped > 0).then_some(FeedItem::Skipped(skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::Direction;

    fn entry(payload: &str) -> TimelineEntry {
        TimelineEntry {
            dir: Direction::Received,
            t_ms: 0,
            size: payload.len() as u64,
            kind: "text".to_owned(),
            payload: payload.to_owned(),
            id: None,
            rtt_ms: None,
        }
    }

    fn payload(item: Option<FeedItem>) -> String {
        match item {
            Some(FeedItem::Entry(e)) => e.payload,
            other => panic!("an entry, not {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_full_feed_skips_entries_and_reports_them_in_place() {
        let (tx, mut rx) = feed_channel_with(2, usize::MAX);
        for p in ["a", "b", "c", "d"] {
            tx.send(entry(p));
        }
        assert_eq!(rx.len(), 2);
        assert_eq!(payload(rx.recv().await), "a");
        // One slot: not enough for the marker and the entry together.
        tx.send(entry("e"));
        assert_eq!(payload(rx.recv().await), "b");
        assert_eq!(rx.recv().await, Some(FeedItem::Skipped(2)));
        tx.send(entry("f"));
        assert_eq!(rx.recv().await, Some(FeedItem::Skipped(1)));
        assert_eq!(payload(rx.recv().await), "f");
        drop(tx);
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn skipped_entries_are_reported_when_the_feed_ends() {
        let (tx, mut rx) = feed_channel_with(1, usize::MAX);
        tx.send(entry("a"));
        tx.send(entry("b"));
        drop(tx);
        assert_eq!(payload(rx.recv().await), "a");
        assert_eq!(rx.recv().await, Some(FeedItem::Skipped(1)));
        assert_eq!(rx.recv().await, None);
    }

    #[test]
    fn the_waiting_bytes_are_bounded_but_one_large_entry_passes() {
        let (tx, mut rx) = feed_channel_with(100, 10);
        let big = "x".repeat(50);
        tx.send(entry(&big));
        tx.send(entry("y"));
        assert_eq!(rx.len(), 1);
        assert_eq!(payload(rx.try_recv()), big);
        assert_eq!(rx.waiting_bytes(), 0);
        assert_eq!(rx.try_recv(), Some(FeedItem::Skipped(1)));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn nothing_is_built_for_a_full_feed() {
        let (tx, _rx) = feed_channel_with(1, usize::MAX);
        tx.send(entry("a"));
        tx.send_with(|| panic!("built although the feed is full"));
    }

    #[test]
    fn a_closed_feed_counts_nothing() {
        let (tx, rx) = feed_channel_with(1, usize::MAX);
        drop(rx);
        tx.send(entry("a"));
        assert_eq!(tx.shared.skipped.load(Ordering::Acquire), 0);
    }
}
