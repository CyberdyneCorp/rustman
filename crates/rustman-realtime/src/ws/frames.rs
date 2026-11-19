//! Turning a resolved frame into the WebSocket message that carries it,
//! and naming received messages for the timeline.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustman_core::vars::{ResolvedBinary, ResolvedFrame};
use tokio_tungstenite::tungstenite::Message;

use crate::error::RealtimeError;

/// The most application data a ping may carry (RFC 6455 5.5).
const MAX_PING_DATA: usize = 125;

/// A message ready to send, with how the timeline shows it.
pub(crate) struct Outgoing {
    pub message: Message,
    pub kind: &'static str,
    pub size: usize,
    /// Text as sent; binary and ping data in base64.
    pub payload: String,
}

/// The message for `frame`: text as is, JSON compact, binary from base64
/// or a file (at most `max_bytes`, checked before the file is read), a
/// ping with base64 data (at most 125 bytes).
pub(crate) async fn outgoing(
    frame: &ResolvedFrame,
    max_bytes: u64,
) -> Result<Outgoing, RealtimeError> {
    let kind = frame.kind();
    let (message, payload) = match frame {
        ResolvedFrame::Text(text) => (Message::text(text.clone()), text.clone()),
        ResolvedFrame::Json(value) => {
            let text = value.to_string();
            (Message::text(text.clone()), text)
        }
        ResolvedFrame::Binary(source) => {
            let bytes = binary(source, max_bytes).await?;
            let shown = STANDARD.encode(&bytes);
            (Message::binary(bytes), shown)
        }
        ResolvedFrame::Ping(data) => {
            let bytes = decode(data, "ping")?;
            if bytes.len() > MAX_PING_DATA {
                let shown = format!("ping data is {} bytes (at most 125)", bytes.len());
                return Err(RealtimeError::Step(shown));
            }
            (Message::Ping(bytes.into()), data.clone())
        }
    };
    Ok(Outgoing {
        size: message.len(),
        message,
        kind,
        payload,
    })
}

async fn binary(source: &ResolvedBinary, max_bytes: u64) -> Result<Vec<u8>, RealtimeError> {
    let file = match source {
        ResolvedBinary::Base64(data) => return decode(data, "binary"),
        ResolvedBinary::File(file) => file,
    };
    let unreadable =
        |e: std::io::Error| RealtimeError::Step(format!("cannot read {}: {e}", file.raw));
    let size = tokio::fs::metadata(&file.path)
        .await
        .map_err(unreadable)?
        .len();
    if size > max_bytes {
        return Err(RealtimeError::Step(format!(
            "{} is {size} bytes, over limits.max_message_bytes ({max_bytes})",
            file.raw
        )));
    }
    tokio::fs::read(&file.path).await.map_err(unreadable)
}

fn decode(data: &str, what: &str) -> Result<Vec<u8>, RealtimeError> {
    STANDARD
        .decode(data.trim())
        .map_err(|e| RealtimeError::Step(format!("invalid base64 in {what} frame: {e}")))
}

/// The timeline type of a received text message: `json` for a JSON
/// object or array, `text` otherwise.
pub(crate) fn text_kind(text: &str) -> &'static str {
    let trimmed = text.trim_start();
    let structured = trimmed.starts_with('{') || trimmed.starts_with('[');
    if structured && serde_json::from_str::<serde_json::Value>(text).is_ok() {
        "json"
    } else {
        "text"
    }
}

/// Binary data as the timeline shows it.
pub(crate) fn shown_binary(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn json_frames_are_compact_text() {
        let frame = ResolvedFrame::Json(json!({"op": "subscribe", "channel": "ticker"}));
        let out = outgoing(&frame, 1024).await.unwrap();
        assert_eq!(
            out.message,
            Message::text(r#"{"op":"subscribe","channel":"ticker"}"#)
        );
        assert_eq!(out.kind, "json");
        assert_eq!(out.size, 37);
    }

    #[tokio::test]
    async fn binary_and_ping_data_are_base64() {
        let frame = ResolvedFrame::Binary(ResolvedBinary::Base64("AAEC".into()));
        let out = outgoing(&frame, 1024).await.unwrap();
        assert_eq!(out.message, Message::binary(vec![0u8, 1, 2]));
        assert_eq!(
            (out.kind, out.size, out.payload.as_str()),
            ("binary", 3, "AAEC")
        );
        let err = outgoing(&ResolvedFrame::Ping("**".into()), 1024)
            .await
            .err()
            .unwrap();
        assert!(
            err.to_string()
                .starts_with("ws: invalid base64 in ping frame"),
            "{err}"
        );
    }

    #[test]
    fn received_text_is_json_only_when_structured() {
        assert_eq!(text_kind(r#"{"a":1}"#), "json");
        assert_eq!(text_kind("[1]"), "json");
        assert_eq!(text_kind("42"), "text");
        assert_eq!(text_kind("{oops"), "text");
    }

    #[tokio::test]
    async fn ping_data_over_125_bytes_is_refused() {
        let fits = STANDARD.encode([7u8; 125]);
        assert!(outgoing(&ResolvedFrame::Ping(fits), 1024).await.is_ok());
        let over = STANDARD.encode([7u8; 126]);
        let err = outgoing(&ResolvedFrame::Ping(over), 1024)
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "ws: ping data is 126 bytes (at most 125)");
    }

    #[tokio::test]
    async fn a_binary_file_over_the_message_limit_is_refused_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [1u8; 64]).unwrap();
        let file = |raw: &str| {
            ResolvedFrame::Binary(ResolvedBinary::File(rustman_core::vars::ResolvedFile {
                raw: raw.to_owned(),
                path: path.clone(),
            }))
        };
        let out = outgoing(&file("./blob.bin"), 64).await.unwrap();
        assert_eq!(out.size, 64);
        let err = outgoing(&file("./blob.bin"), 63).await.err().unwrap();
        assert_eq!(
            err.to_string(),
            "ws: ./blob.bin is 64 bytes, over limits.max_message_bytes (63)"
        );
    }
}
