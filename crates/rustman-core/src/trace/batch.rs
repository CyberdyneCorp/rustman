//! The bounded span queue and the warning limiter of the export pipeline
//! (trace-propagation spec "Export failures never affect requests").

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Spans waiting for export: at most [`SpanQueue::CAPACITY`], the oldest
/// dropped first when full.
#[derive(Debug)]
pub struct SpanQueue<T> {
    items: VecDeque<T>,
    capacity: usize,
    dropped: u64,
}

impl<T> Default for SpanQueue<T> {
    fn default() -> Self {
        Self::with_capacity(Self::CAPACITY)
    }
}

impl<T> SpanQueue<T> {
    pub const CAPACITY: usize = 2048;

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    /// Queue `item`, dropping the oldest one when full.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
            self.dropped += 1;
        }
        self.items.push_back(item);
    }

    /// Everything queued, oldest first.
    pub fn drain(&mut self) -> Vec<T> {
        self.items.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many spans were dropped so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Lets one warning through per interval (a minute by default).
#[derive(Debug)]
pub struct WarnLimiter {
    every: Duration,
    last: Option<Instant>,
}

impl Default for WarnLimiter {
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

impl WarnLimiter {
    pub fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    /// Whether a warning at `now` may be shown.
    pub fn allow(&mut self, now: Instant) -> bool {
        let quiet = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.every);
        if !quiet {
            self.last = Some(now);
        }
        !quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut queue = SpanQueue::with_capacity(3);
        for i in 0..5 {
            queue.push(i);
        }
        assert_eq!((queue.len(), queue.dropped()), (3, 2));
        assert_eq!(queue.drain(), [2, 3, 4]);
        assert!(queue.is_empty());
        let default: SpanQueue<u8> = SpanQueue::default();
        assert_eq!(default.capacity, 2048);
    }

    #[test]
    fn one_warning_per_minute() {
        let mut limiter = WarnLimiter::default();
        let start = Instant::now();
        let shown = (0..120)
            .filter(|s| limiter.allow(start + Duration::from_millis(s * 500)))
            .count();
        assert_eq!(shown, 1, "one warning in the first minute");
        assert!(limiter.allow(start + Duration::from_secs(60)));
        assert!(!limiter.allow(start + Duration::from_secs(61)));
        assert!(limiter.allow(start + Duration::from_secs(121)));
    }
}
