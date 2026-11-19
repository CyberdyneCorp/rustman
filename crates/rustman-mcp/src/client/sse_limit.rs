//! A size limit for the server-sent events an MCP server streams: an event
//! is buffered whole before it is parsed, so without a limit one endless
//! event could grow the client's memory without bound. The limit counts
//! the bytes of an event's field lines (comments excluded, as rmcp does)
//! and ends the stream with an error once one event passes it.

use bytes::Bytes;
use futures_util::stream::{BoxStream, Stream, StreamExt};
use rmcp::transport::streamable_http_client::SseError;
use sse_stream::{Sse, SseStream};

/// The largest event accepted, as rmcp's own clients default to.
pub(crate) const MAX_SSE_EVENT: usize = 16 * 1024 * 1024;

/// Tracks the size of the event being received.
#[derive(Debug)]
struct EventSize {
    max: usize,
    /// The field lines of the event so far, with their newlines.
    event: usize,
    /// The line being received.
    line: usize,
    comment: bool,
    after_cr: bool,
}

impl EventSize {
    fn new(max: usize) -> Self {
        Self {
            max,
            event: 0,
            line: 0,
            comment: false,
            after_cr: false,
        }
    }

    /// Account for `chunk`; `false` once the event is too large.
    fn observe(&mut self, chunk: &[u8]) -> bool {
        chunk.iter().all(|&byte| self.byte(byte))
    }

    fn byte(&mut self, byte: u8) -> bool {
        let after_cr = std::mem::take(&mut self.after_cr);
        match byte {
            b'\n' if after_cr => {}
            b'\r' | b'\n' => {
                self.end_line();
                self.after_cr = byte == b'\r';
            }
            _ => {
                if self.line == 0 {
                    self.comment = byte == b':';
                }
                self.line += 1;
            }
        }
        self.event.saturating_add(self.line) <= self.max
    }

    fn end_line(&mut self) {
        if self.line == 0 {
            self.event = 0;
        } else if !self.comment {
            self.event = self.event.saturating_add(self.line + 1);
        }
        self.line = 0;
        self.comment = false;
    }
}

/// Parse `chunks` as server-sent events, failing (and ending the stream)
/// with "SSE event larger than `max` bytes" when one event passes `max`.
pub(crate) fn limited_events<S, E>(
    chunks: S,
    max: usize,
) -> BoxStream<'static, Result<Sse, SseError>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: From<String> + std::error::Error + Send + Sync + 'static,
{
    let mut size = EventSize::new(max);
    let checked = chunks
        .map(move |chunk| match chunk {
            Ok(bytes) if !size.observe(&bytes) => {
                Err(E::from(format!("SSE event larger than {max} bytes")))
            }
            other => other,
        })
        .scan(false, |failed, chunk| {
            let item = (!*failed).then(|| {
                *failed = chunk.is_err();
                chunk
            });
            std::future::ready(item)
        });
    SseStream::from_bytes_stream(checked).boxed()
}

#[cfg(test)]
mod tests {
    use futures_util::stream;

    use super::*;

    fn chunks(items: Vec<&'static [u8]>) -> impl Stream<Item = Result<Bytes, Failure>> + Send {
        stream::iter(items.into_iter().map(|b| Ok(Bytes::from_static(b))))
    }

    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    struct Failure(String);

    impl From<String> for Failure {
        fn from(message: String) -> Self {
            Self(message)
        }
    }

    async fn collect(items: Vec<&'static [u8]>, max: usize) -> Vec<Result<Sse, String>> {
        limited_events(chunks(items), max)
            .map(|event| event.map_err(|e| e.to_string()))
            .collect()
            .await
    }

    #[tokio::test]
    async fn events_within_the_limit_pass() {
        let events = collect(
            vec![
                b"data: one\n\n",
                b":ka\n:ka\n:ka\n:ka\n\n",
                b"data: two\r\n\r\n",
            ],
            10,
        )
        .await;
        let data: Vec<_> = events.into_iter().filter_map(|e| e.unwrap().data).collect();
        assert_eq!(data, vec!["one", "two"]);
    }

    #[tokio::test]
    async fn an_oversized_event_ends_the_stream_with_an_error() {
        let big: &'static [u8] = Box::leak(vec![b'x'; 1 << 20].into_boxed_slice());
        let events = collect(
            vec![b"data: ok\n\n", b"data: ", big, big, b"\n\n"],
            64 * 1024,
        )
        .await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].as_ref().unwrap().data.as_deref(), Some("ok"));
        let err = events[1].as_ref().unwrap_err();
        assert!(err.contains("SSE event larger than 65536 bytes"), "{err}");
    }

    #[test]
    fn the_size_counts_field_lines_of_one_event() {
        let mut size = EventSize::new(15);
        assert!(size.observe(b"data: a\ndata: b"));
        assert!(!size.observe(b"c"));
        let mut size = EventSize::new(8);
        assert!(size.observe(b"data: a\n\ndata: b\n\n:abc\n:abc\n:abc\n"));
    }
}
