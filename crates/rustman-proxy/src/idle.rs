//! Idle detection for long-lived connections (opaque tunnels and
//! intercepted HTTP/1.1 or HTTP/2 connections): every byte read or written
//! marks the connection active, and [`Activity::idle`] completes once no
//! byte moved for the configured time.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Instant;

/// When a connection last moved a byte; shared by the streams of one
/// connection.
#[derive(Debug)]
pub(crate) struct Activity {
    start: Instant,
    /// Milliseconds after `start`.
    last: AtomicU64,
}

impl Activity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            last: AtomicU64::new(0),
        })
    }

    fn touch(&self) {
        let now = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last.fetch_max(now, Ordering::Relaxed);
    }

    fn last(&self) -> Instant {
        self.start + Duration::from_millis(self.last.load(Ordering::Relaxed))
    }

    /// Completes once no byte moved for `limit`.
    pub async fn idle(&self, limit: Duration) {
        loop {
            let deadline = self.last() + limit;
            if Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep_until(deadline).await;
        }
    }
}

/// A stream that counts the bytes read from it and marks its connection
/// active on every read and write.
pub(crate) struct Tracked<S> {
    inner: S,
    read: Arc<AtomicU64>,
    activity: Arc<Activity>,
}

impl<S> Tracked<S> {
    pub fn new(inner: S, activity: &Arc<Activity>) -> Self {
        Self {
            inner,
            read: Arc::default(),
            activity: Arc::clone(activity),
        }
    }

    /// The bytes read so far, shared.
    pub fn read_count(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.read)
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Tracked<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        if read > 0 {
            self.read.fetch_add(read as u64, Ordering::Relaxed);
            self.activity.touch();
        }
        poll
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tracked<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(poll, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.touch();
        }
        poll
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn idle_completes_only_after_the_limit_without_traffic() {
        let activity = Activity::new();
        let (client, mut server) = tokio::io::duplex(64);
        let mut tracked = Tracked::new(client, &activity);
        let limit = Duration::from_secs(300);
        let idle = activity.idle(limit);
        tokio::pin!(idle);
        tokio::time::advance(Duration::from_secs(200)).await;
        server.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        tracked.read_exact(&mut buf).await.unwrap();
        assert_eq!(tracked.read_count().load(Ordering::Relaxed), 4);
        let early = tokio::time::timeout(Duration::from_secs(299), &mut idle).await;
        assert!(early.is_err(), "traffic at 200 s keeps it open until 500 s");
        tokio::time::timeout(Duration::from_secs(2), &mut idle)
            .await
            .expect("idle after 300 s without traffic");
    }
}
