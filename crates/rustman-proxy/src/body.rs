//! Bodies relayed frame by frame: the client gets every byte as it
//! arrives, while a copy of at most `max_body` bytes is kept for the
//! capture (spec: Capture limits). Nothing is buffered beyond that copy,
//! and the copies of all bodies in flight share one [`Budget`]: past it,
//! a copy stops growing and is marked truncated while the body still
//! flows.

use std::error::Error as StdError;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};

use crate::capture::BodyCapture;

pub type BoxError = Box<dyn StdError + Send + Sync>;

/// The body type of every response the proxy sends.
pub type ProxyBody = BoxBody<Bytes, BoxError>;

/// A body with nothing in it.
pub fn empty() -> ProxyBody {
    Empty::new().map_err(|never| match never {}).boxed()
}

/// A body holding `bytes`.
pub fn full(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

/// The bytes the copies of all bodies in flight may hold together;
/// cloning shares it.
#[derive(Clone, Debug)]
pub struct Budget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

impl Budget {
    pub fn new(limit: usize) -> Self {
        Self {
            used: Arc::default(),
            limit,
        }
    }

    /// The bytes held now.
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// Reserve up to `want` bytes; returns how many were granted.
    fn take(&self, want: usize) -> usize {
        let mut granted = 0;
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                granted = want.min(self.limit.saturating_sub(used));
                Some(used + granted)
            });
        granted
    }

    fn give_back(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// The in-flight copy of one body; the bytes it reserved from its
/// [`Budget`] are returned when it is dropped.
#[derive(Debug)]
pub struct Buffer {
    capture: BodyCapture,
    reserved: usize,
    budget: Budget,
}

impl Buffer {
    pub fn new(budget: Budget) -> Self {
        Self {
            capture: BodyCapture::default(),
            reserved: 0,
            budget,
        }
    }

    /// Add `chunk`, keeping at most `limit` bytes and no more than the
    /// budget grants. Once a chunk is cut, nothing more is kept: a capture
    /// holds a prefix of the body.
    fn push(&mut self, chunk: &[u8], limit: usize) {
        let room = limit
            .saturating_sub(self.capture.bytes.len())
            .min(chunk.len());
        let granted = if self.capture.truncated {
            0
        } else {
            self.budget.take(room)
        };
        self.reserved += granted;
        let kept = self.capture.bytes.len() + granted;
        self.capture.push(chunk, kept);
    }

    /// The copy so far.
    pub fn capture(&self) -> BodyCapture {
        self.capture.clone()
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.budget.give_back(self.reserved);
    }
}

/// The shared copy of one body.
pub type Sink = Arc<Mutex<Buffer>>;

/// A new, empty copy drawing on `budget`.
pub fn sink(budget: &Budget) -> Sink {
    Arc::new(Mutex::new(Buffer::new(budget.clone())))
}

/// Run once, when the body is dropped (finished, failed or abandoned).
type OnDrop = Box<dyn FnOnce() + Send + Sync>;

/// A body that passes `inner` through and copies it into `sink`.
pub struct Tee<B> {
    inner: B,
    sink: Sink,
    limit: usize,
    on_drop: Option<OnDrop>,
}

impl<B> Tee<B> {
    pub fn new(inner: B, sink: Sink, limit: usize) -> Self {
        Self {
            inner,
            sink,
            limit,
            on_drop: None,
        }
    }

    /// Call `f` once this body is dropped.
    #[must_use]
    pub fn on_drop(mut self, f: impl FnOnce() + Send + Sync + 'static) -> Self {
        self.on_drop = Some(Box::new(f));
        self
    }
}

impl<B> Drop for Tee<B> {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

impl<B> Body for Tee<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let frame = ready!(Pin::new(&mut self.inner).poll_frame(cx));
        Poll::Ready(match frame {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    self.sink
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(data, self.limit);
                }
                Some(Ok(frame))
            }
            Some(Err(err)) => Some(Err(err.into())),
            None => None,
        })
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn the_client_gets_everything_and_the_copy_is_capped() {
        let sink = sink(&Budget::new(usize::MAX));
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&dropped);
        let tee = Tee::new(
            Full::new(Bytes::from_static(b"0123456789")),
            sink.clone(),
            4,
        )
        .on_drop(move || flag.store(true, Ordering::SeqCst));
        let all = tee.collect().await.unwrap().to_bytes();
        assert_eq!(&all[..], b"0123456789");
        let copy = sink.lock().unwrap().capture();
        assert_eq!((copy.bytes.as_slice(), copy.total), (&b"0123"[..], 10));
        assert!(copy.truncated);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn copies_past_the_shared_budget_are_truncated_and_the_body_still_flows() {
        let budget = Budget::new(6);
        let first = sink(&budget);
        let second = sink(&budget);
        let body = |text: &'static [u8], sink: &Sink| {
            Tee::new(Full::new(Bytes::from_static(text)), sink.clone(), 100)
        };
        let all = body(b"abcd", &first).collect().await.unwrap().to_bytes();
        assert_eq!(&all[..], b"abcd");
        let all = body(b"0123456789", &second)
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&all[..], b"0123456789", "the client gets every byte");
        let copy = second.lock().unwrap().capture();
        assert_eq!((copy.bytes.as_slice(), copy.total), (&b"01"[..], 10));
        assert!(copy.truncated);
        assert!(!first.lock().unwrap().capture().truncated);
        assert_eq!(budget.used(), 6);
        drop(first);
        drop(second);
        assert_eq!(budget.used(), 0, "dropped copies return their bytes");
    }

    #[test]
    fn a_cut_copy_keeps_a_prefix_even_when_the_budget_frees_up() {
        let budget = Budget::new(4);
        let hog = sink(&budget);
        hog.lock().unwrap().push(b"wxyz", 100);
        let mut buffer = Buffer::new(budget.clone());
        buffer.push(b"ab", 100);
        drop(hog);
        buffer.push(b"cd", 100);
        let copy = buffer.capture();
        assert!(copy.bytes.is_empty() && copy.truncated && copy.total == 4);
    }
}
