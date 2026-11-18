//! Line framing: one JSON message per line, at most [`MAX_LINE_BYTES`]
//! bytes per line (newline excluded). Bodies too large for a line are
//! spilled to a file and referenced by a [`BlobRef`]; the spill itself is
//! the caller's I/O.

use crate::message::{BlobRef, Body, InspectResponse, Message};

/// The longest line either side writes or accepts: 16 MiB.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Why a line could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("line of {0} bytes exceeds the {MAX_LINE_BYTES}-byte limit")]
    TooLong(usize),
    #[error("invalid message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("line is not valid UTF-8")]
    NotUtf8,
}

/// The line for `message`, ending in `\n`.
pub fn encode_line(message: &Message) -> Result<String, FrameError> {
    let mut line = serde_json::to_string(message)?;
    if line.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong(line.len()));
    }
    line.push('\n');
    Ok(line)
}

/// The line for an `inspect_response`. When the body does not fit on one
/// line, `spill` writes it to a file and returns the reference sent in its
/// place.
pub fn encode_inspect<E>(
    inspect: InspectResponse,
    spill: impl FnOnce(&str) -> Result<BlobRef, E>,
) -> Result<Result<String, FrameError>, E> {
    let message = Message::InspectResponse(Box::new(inspect));
    match encode_line(&message) {
        Err(FrameError::TooLong(_)) => {}
        other => return Ok(other),
    }
    let Message::InspectResponse(mut inspect) = message else {
        unreachable!("built above");
    };
    if let Body::Inline { text } = &inspect.body {
        inspect.body = Body::BlobRef(spill(text)?);
    }
    Ok(encode_line(&Message::InspectResponse(inspect)))
}

/// The message on one line (a trailing `\n` or `\r\n` is ignored).
pub fn decode_line(line: &str) -> Result<Message, FrameError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong(line.len()));
    }
    Ok(serde_json::from_str(line)?)
}

/// Splits a byte stream into messages. A line longer than the limit is
/// reported once and skipped without being buffered whole.
#[derive(Debug, Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
    /// Bytes of an over-long line seen so far, while it is being skipped.
    skipping: Option<usize>,
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every message (or error) of the lines completed.
    pub fn push(&mut self, mut bytes: &[u8]) -> Vec<Result<Message, FrameError>> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let end = bytes.iter().position(|&b| b == b'\n');
            let (chunk, rest) = match end {
                Some(i) => (&bytes[..i], &bytes[i + 1..]),
                None => (bytes, &[][..]),
            };
            self.take(chunk);
            if end.is_some() {
                out.extend(self.finish_line());
            }
            bytes = rest;
        }
        out
    }

    fn take(&mut self, chunk: &[u8]) {
        if let Some(seen) = &mut self.skipping {
            *seen += chunk.len();
            return;
        }
        if self.buf.len() + chunk.len() > MAX_LINE_BYTES + 1 {
            self.skipping = Some(self.buf.len() + chunk.len());
            self.buf = Vec::new();
            return;
        }
        self.buf.extend_from_slice(chunk);
    }

    fn finish_line(&mut self) -> Option<Result<Message, FrameError>> {
        if let Some(seen) = self.skipping.take() {
            return Some(Err(FrameError::TooLong(seen)));
        }
        let line = std::mem::take(&mut self.buf);
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        Some(match String::from_utf8(line) {
            Ok(text) => decode_line(&text),
            Err(_) => Err(FrameError::NotUtf8),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Close, Hello};

    fn inspect(text: String) -> InspectResponse {
        InspectResponse {
            request: None,
            status: Some(200),
            content_type: None,
            headers: Vec::new(),
            selector: None,
            body: Body::Inline { text },
        }
    }

    /// An inspect message whose line is exactly `len` bytes long.
    fn inspect_of_len(len: usize) -> InspectResponse {
        let empty = encode_line(&Message::InspectResponse(Box::new(inspect(String::new()))));
        let overhead = empty.unwrap().len() - 1;
        inspect("a".repeat(len - overhead))
    }

    fn line_len(message: &InspectResponse) -> usize {
        serde_json::to_string(&Message::InspectResponse(Box::new(message.clone())))
            .unwrap()
            .len()
    }

    #[test]
    fn lines_end_in_a_newline_and_decode_back() {
        let hello = Message::Hello(Hello::new("rustman 0.2.0"));
        let line = encode_line(&hello).unwrap();
        assert_eq!(
            line,
            "{\"type\":\"hello\",\"protocol\":\"rustman-viz/1\",\"producer\":\"rustman 0.2.0\"}\n"
        );
        assert_eq!(decode_line(&line).unwrap(), hello);
        assert_eq!(
            decode_line("{\"type\":\"close\"}\r\n").unwrap(),
            Message::Close(Close {})
        );
    }

    #[test]
    fn the_limit_is_16_mib_per_line() {
        let fits = inspect_of_len(MAX_LINE_BYTES - 1);
        assert_eq!(line_len(&fits), MAX_LINE_BYTES - 1);
        let line = encode_line(&Message::InspectResponse(Box::new(fits))).unwrap();
        assert_eq!(line.len(), MAX_LINE_BYTES);
        assert!(decode_line(&line).is_ok());

        let over = inspect_of_len(MAX_LINE_BYTES + 1);
        assert_eq!(line_len(&over), MAX_LINE_BYTES + 1);
        let message = Message::InspectResponse(Box::new(over));
        assert!(matches!(
            encode_line(&message),
            Err(FrameError::TooLong(n)) if n == MAX_LINE_BYTES + 1
        ));
        let raw = serde_json::to_string(&message).unwrap();
        assert!(matches!(decode_line(&raw), Err(FrameError::TooLong(_))));
    }

    #[test]
    fn oversized_bodies_spill_to_a_blob_ref() {
        let blob = BlobRef {
            path: "/tmp/body.json".into(),
            bytes: (MAX_LINE_BYTES + 1) as u64,
            media_type: Some("application/json".into()),
        };
        let fits = inspect_of_len(MAX_LINE_BYTES - 1);
        let mut called = false;
        let line = encode_inspect::<()>(fits, |_| {
            called = true;
            Ok(blob.clone())
        });
        assert!(line.unwrap().is_ok() && !called, "16 MiB - 1 stays inline");

        let over = inspect_of_len(MAX_LINE_BYTES + 1);
        let line = encode_inspect::<()>(over, |text| {
            assert_eq!(
                text.len(),
                MAX_LINE_BYTES + 1 - line_len(&inspect(String::new()))
            );
            Ok(blob.clone())
        });
        let line = line.unwrap().unwrap();
        assert!(line.len() < 1024, "{line}");
        let Message::InspectResponse(sent) = decode_line(&line).unwrap() else {
            panic!("expected inspect_response");
        };
        assert_eq!(sent.body, Body::BlobRef(blob));
        let failed = encode_inspect(inspect_of_len(MAX_LINE_BYTES + 1), |_| Err("disk full"));
        assert_eq!(failed.unwrap_err(), "disk full");
    }

    #[test]
    fn the_stream_decoder_splits_lines_across_chunks() {
        let mut decoder = LineDecoder::new();
        let line = encode_line(&Message::Close(Close {})).unwrap();
        let (a, b) = line.as_bytes().split_at(5);
        assert!(decoder.push(a).is_empty());
        let got = decoder.push(&[b, b"\n", b"{\"type\":\"futuristic\",\"x\":1}\n"].concat());
        let got: Vec<_> = got.into_iter().map(Result::unwrap).collect();
        assert_eq!(got, [Message::Close(Close {}), Message::Unknown]);
        let bad = decoder.push(b"\xff\xfe\nnot json\n");
        assert!(matches!(bad[0], Err(FrameError::NotUtf8)));
        assert!(matches!(bad[1], Err(FrameError::Json(_))));
    }

    #[test]
    fn the_stream_decoder_skips_an_over_long_line_and_recovers() {
        let mut decoder = LineDecoder::new();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..17 {
            assert!(decoder.push(&chunk).is_empty());
        }
        assert!(decoder.buf.capacity() <= MAX_LINE_BYTES + 1);
        let out = decoder.push(b"\n{\"type\":\"close\"}\n");
        assert!(matches!(out[0], Err(FrameError::TooLong(n)) if n == 17 * 1024 * 1024));
        assert_eq!(out[1].as_ref().unwrap(), &Message::Close(Close {}));
    }
}
