//! The capture ring buffer (design D3, spec: Bounded capture buffer) and the
//! event channel every front-end subscribes to.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast;

use crate::capture::Capture;
use crate::config::BufferLimits;
use crate::event::Event;

/// Events kept for a slow subscriber before it starts missing some.
const EVENT_BACKLOG: usize = 1024;

/// Captures in arrival order, bounded by count and total body bytes, shared
/// by the listener and its front-ends. Cloning shares the same buffer.
#[derive(Clone, Debug)]
pub struct CaptureStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    buffer: Mutex<Buffer>,
    events: broadcast::Sender<Event>,
}

#[derive(Debug)]
struct Buffer {
    captures: VecDeque<Arc<Capture>>,
    bytes: usize,
    next_id: u64,
    limits: BufferLimits,
}

impl Buffer {
    fn over_limits(&self) -> bool {
        self.captures.len() > self.limits.max_captures || self.bytes > self.limits.max_buffer_bytes
    }

    /// Drop the oldest captures until both limits hold. The newest capture
    /// is always kept, even when its body alone exceeds the byte budget.
    fn evict(&mut self) {
        while self.over_limits() && self.captures.len() > 1 {
            if let Some(old) = self.captures.pop_front() {
                self.bytes -= old.body_size();
            }
        }
    }
}

impl CaptureStore {
    pub fn new(limits: BufferLimits) -> Self {
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let buffer = Buffer {
            captures: VecDeque::new(),
            bytes: 0,
            next_id: 1,
            limits,
        };
        Self {
            inner: Arc::new(Inner {
                buffer: Mutex::new(buffer),
                events,
            }),
        }
    }

    fn buffer(&self) -> MutexGuard<'_, Buffer> {
        self.inner
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Store `capture` with the next sequence id, evict what no longer fits
    /// and announce it as [`Event::Captured`].
    pub fn push(&self, mut capture: Capture) -> Arc<Capture> {
        let stored = {
            let mut buffer = self.buffer();
            capture.id = buffer.next_id;
            buffer.next_id += 1;
            let stored = Arc::new(capture);
            buffer.bytes += stored.body_size();
            buffer.captures.push_back(Arc::clone(&stored));
            buffer.evict();
            stored
        };
        self.emit(Event::Captured(Arc::clone(&stored)));
        stored
    }

    /// Empty the buffer; ids keep counting up.
    pub fn clear(&self) {
        {
            let mut buffer = self.buffer();
            buffer.captures.clear();
            buffer.bytes = 0;
        }
        self.emit(Event::Cleared);
    }

    /// Every capture, oldest first.
    pub fn snapshot(&self) -> Vec<Arc<Capture>> {
        self.buffer().captures.iter().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Arc<Capture>> {
        self.buffer().captures.iter().find(|c| c.id == id).cloned()
    }

    pub fn len(&self) -> usize {
        self.buffer().captures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Body bytes currently held.
    pub fn total_bytes(&self) -> usize {
        self.buffer().bytes
    }

    pub fn limits(&self) -> BufferLimits {
        self.buffer().limits
    }

    /// Events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Announce `event` to every subscriber (none is fine).
    pub fn emit(&self, event: Event) {
        let _ = self.inner.events.send(event);
    }
}

impl Default for CaptureStore {
    fn default() -> Self {
        Self::new(BufferLimits::default())
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::capture::tests::capture;

    const MIB: usize = 1024 * 1024;

    fn ids(store: &CaptureStore) -> Vec<u64> {
        store.snapshot().iter().map(|c| c.id).collect()
    }

    #[test]
    fn the_501st_capture_evicts_the_first() {
        let store = CaptureStore::default();
        for _ in 0..500 {
            store.push(capture("/a", b"x"));
        }
        assert_eq!(store.len(), 500);
        assert_eq!(ids(&store).first(), Some(&1));
        store.push(capture("/a", b"x"));
        assert_eq!(store.len(), 500);
        assert_eq!(ids(&store).first(), Some(&2));
        assert_eq!(ids(&store).last(), Some(&501));
        assert_eq!(store.total_bytes(), 500);
    }

    #[test]
    fn exceeding_the_byte_budget_evicts_oldest_until_under_it() {
        let store = CaptureStore::default();
        // One shared 9 MiB allocation: every capture counts its full length.
        let body = Bytes::from(vec![0u8; 9 * MIB]);
        for _ in 0..30 {
            let mut c = capture("/big", b"");
            c.body = body.clone();
            store.push(c);
        }
        assert!(store.total_bytes() <= 256 * MIB);
        // 28 x 9 MiB = 252 MiB fits; 29 would be 261 MiB.
        assert_eq!(store.len(), 28);
        assert_eq!(ids(&store).first(), Some(&3));
        assert_eq!(store.total_bytes(), 28 * 9 * MIB);
    }

    #[test]
    fn the_newest_capture_is_kept_even_when_alone_over_budget() {
        let store = CaptureStore::new(BufferLimits {
            max_captures: 10,
            max_buffer_bytes: 4,
        });
        store.push(capture("/a", b"ab"));
        store.push(capture("/b", b"abcdef"));
        assert_eq!(ids(&store), [2]);
    }

    #[test]
    fn clear_empties_and_ids_keep_counting() {
        let store = CaptureStore::default();
        let mut events = store.subscribe();
        store.push(capture("/a", b"abc"));
        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
        let next = store.push(capture("/a", b""));
        assert_eq!(next.id, 2);
        assert!(matches!(events.try_recv(), Ok(Event::Captured(c)) if c.id == 1));
        assert_eq!(events.try_recv().ok(), Some(Event::Cleared));
        assert_eq!(store.get(2).map(|c| c.path.clone()).as_deref(), Some("/a"));
        assert!(store.get(1).is_none());
    }
}
