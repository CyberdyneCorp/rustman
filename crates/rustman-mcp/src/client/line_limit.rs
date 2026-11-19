//! A size limit for the messages a stdio server writes: one JSON-RPC
//! message per line, so without a limit one endless line could grow the
//! client's memory without bound. The limit matches the SSE one
//! ([`super::sse_limit::MAX_SSE_EVENT`]); a longer line ends the stream with
//! an error, which closes the session, and the reason is kept for the
//! requests that fail because of it.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, ReadBuf};

use super::sse_limit::MAX_SSE_EVENT;

/// The longest message line accepted from a stdio server, in bytes.
pub(crate) const MAX_STDIO_MESSAGE: usize = MAX_SSE_EVENT;

/// Why the client ended a connection itself, once it did. Clones share it.
pub(crate) type EndReason = Arc<OnceLock<String>>;

/// `inner`, failing once a line grows past `max` bytes (newline excluded).
pub(crate) struct LineLimit<R> {
    inner: R,
    max: usize,
    /// The bytes of the line being read.
    line: usize,
    ended: EndReason,
}

impl<R> LineLimit<R> {
    pub(crate) fn new(inner: R, max: usize, ended: EndReason) -> Self {
        Self {
            inner,
            max,
            line: 0,
            ended,
        }
    }

    /// Account for `chunk`; `false` once the line is too long.
    fn observe(&mut self, chunk: &[u8]) -> bool {
        let mut pieces = chunk.split(|b| *b == b'\n');
        let first = pieces.next().unwrap_or_default();
        self.line = self.line.saturating_add(first.len());
        if self.line > self.max {
            return false;
        }
        for piece in pieces {
            self.line = piece.len();
            if self.line > self.max {
                return false;
            }
        }
        true
    }
}

/// "message larger than 16 MiB" for the default limit.
fn too_large(max: usize) -> String {
    let size = if max.is_multiple_of(1024 * 1024) {
        format!("{} MiB", max / (1024 * 1024))
    } else {
        format!("{max} bytes")
    };
    format!("the server sent a message larger than {size}; the session was closed")
}

impl<R: AsyncRead + Unpin> AsyncRead for LineLimit<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        ready!(Pin::new(&mut self.inner).poll_read(cx, buf))?;
        if self.observe(&buf.filled()[before..]) {
            return Poll::Ready(Ok(()));
        }
        // A failed read reports nothing read.
        buf.set_filled(before);
        let reason = too_large(self.max);
        let _ = self.ended.set(reason.clone());
        Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, reason)))
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;

    async fn read_all(input: &[u8], max: usize) -> (io::Result<Vec<u8>>, EndReason) {
        let ended = EndReason::default();
        let mut limited = LineLimit::new(input, max, ended.clone());
        let mut out = Vec::new();
        let read = limited.read_to_end(&mut out).await.map(|_| out);
        (read, ended)
    }

    #[tokio::test]
    async fn lines_up_to_the_limit_pass() {
        let (read, ended) = read_all(b"12345\n1234\n12345", 5).await;
        assert_eq!(read.unwrap(), b"12345\n1234\n12345");
        assert!(ended.get().is_none());
    }

    #[tokio::test]
    async fn a_longer_line_ends_the_stream_with_the_reason() {
        let (read, ended) = read_all(b"ok\n123456\n", 5).await;
        let err = read.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            ended.get().unwrap(),
            "the server sent a message larger than 5 bytes; the session was closed"
        );
    }

    #[test]
    fn a_line_split_across_reads_counts_whole() {
        let mut limit = LineLimit::new(&b""[..], 5, EndReason::default());
        assert!(limit.observe(b"abc"));
        assert!(limit.observe(b"de\nab"));
        assert!(limit.observe(b"c"));
        assert!(!limit.observe(b"def"));
    }

    #[test]
    fn the_default_limit_is_16_mib() {
        assert_eq!(MAX_STDIO_MESSAGE, 16 * 1024 * 1024);
        assert_eq!(
            too_large(MAX_STDIO_MESSAGE),
            "the server sent a message larger than 16 MiB; the session was closed"
        );
    }
}
