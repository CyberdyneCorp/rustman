//! The capture list shared by the proxy and its front-ends: bounded by a
//! number of captures and by the bytes their bodies take, oldest evicted
//! first, with a broadcast of every new capture for live output.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use tokio::sync::broadcast;

use crate::capture::{Capture, Recorded};

/// How many new captures a slow subscriber may lag behind.
const BROADCAST_CAPACITY: usize = 1024;

/// The capture list; cloning shares it.
#[derive(Clone)]
pub struct CaptureStore {
    inner: Arc<Inner>,
}

struct Inner {
    captures: Mutex<Buffer>,
    next_id: AtomicU64,
    max_captures: usize,
    max_bytes: usize,
    live: broadcast::Sender<Arc<Capture>>,
}

#[derive(Default)]
struct Buffer {
    entries: VecDeque<Arc<Capture>>,
    bytes: usize,
}

/// Shows the size only: captures hold unredacted exchanges.
impl std::fmt::Debug for CaptureStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureStore")
            .field("captures", &self.len())
            .finish_non_exhaustive()
    }
}

impl CaptureStore {
    /// A store keeping at most `max_captures` captures whose bodies take at
    /// most `max_bytes` together.
    pub fn new(max_captures: usize, max_bytes: usize) -> Self {
        let (live, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                captures: Mutex::new(Buffer::default()),
                next_id: AtomicU64::new(1),
                max_captures: max_captures.max(1),
                max_bytes,
                live,
            }),
        }
    }

    /// Record `recorded`, started at `started`, and announce it.
    pub fn push(&self, started: SystemTime, recorded: Recorded) -> Arc<Capture> {
        let capture = Arc::new(Capture {
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed),
            started,
            recorded,
        });
        {
            let mut buffer = self.lock();
            buffer.bytes += capture.stored_bytes();
            buffer.entries.push_back(Arc::clone(&capture));
            self.evict(&mut buffer);
        }
        let _ = self.inner.live.send(Arc::clone(&capture));
        capture
    }

    fn evict(&self, buffer: &mut Buffer) {
        while buffer.entries.len() > self.inner.max_captures
            || (buffer.bytes > self.inner.max_bytes && buffer.entries.len() > 1)
        {
            let Some(old) = buffer.entries.pop_front() else {
                return;
            };
            buffer.bytes -= old.stored_bytes();
        }
    }

    /// Every capture kept, oldest first.
    pub fn snapshot(&self) -> Vec<Arc<Capture>> {
        self.lock().entries.iter().cloned().collect()
    }

    /// New captures from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Capture>> {
        self.inner.live.subscribe()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Buffer> {
        self.inner
            .captures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::capture::{BodyCapture, Exchange, Tunnel};

    fn tunnel(port: u16) -> Recorded {
        Recorded::Tunnel(Tunnel {
            host: "h".into(),
            port,
            bytes_up: 0,
            bytes_down: 0,
            duration: Duration::ZERO,
            error: None,
        })
    }

    fn sized(bytes: usize) -> Recorded {
        Recorded::Exchange(Box::new(Exchange {
            method: "GET".into(),
            url: "http://h/".into(),
            version: "HTTP/1.1".into(),
            request_headers: Vec::new(),
            request_body: BodyCapture::default(),
            status: Some(200),
            response_headers: Vec::new(),
            response_body: BodyCapture {
                bytes: vec![0; bytes],
                total: bytes as u64,
                truncated: false,
            },
            duration: Duration::ZERO,
            intercepted: false,
            error: None,
        }))
    }

    #[test]
    fn the_oldest_captures_are_evicted_past_the_count() {
        let store = CaptureStore::new(2, usize::MAX);
        for port in 1..=3 {
            store.push(SystemTime::now(), tunnel(port));
        }
        let ids: Vec<u64> = store.snapshot().iter().map(|c| c.id).collect();
        assert_eq!(ids, [2, 3]);
    }

    #[test]
    fn the_oldest_captures_are_evicted_past_the_bytes() {
        let store = CaptureStore::new(100, 25);
        store.push(SystemTime::now(), sized(10));
        store.push(SystemTime::now(), sized(10));
        store.push(SystemTime::now(), sized(10));
        let ids: Vec<u64> = store.snapshot().iter().map(|c| c.id).collect();
        assert_eq!(ids, [2, 3]);
    }

    #[tokio::test]
    async fn subscribers_see_new_captures() {
        let store = CaptureStore::new(10, usize::MAX);
        let mut live = store.subscribe();
        store.push(SystemTime::now(), tunnel(1));
        assert_eq!(live.recv().await.unwrap().id, 1);
        assert_eq!(store.len(), 1);
    }
}
