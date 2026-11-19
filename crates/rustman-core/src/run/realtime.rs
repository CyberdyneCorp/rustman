//! What a realtime session adds to its request record (realtime-sessions
//! "History recording", design D5): the protocol, the handshake, the
//! timeline (capped by `max_timeline`, with the number of dropped
//! entries), and how the session ended.
//!
//! Records are redacted with [`RealtimeRecord::redacted`] before they reach
//! any sink. Redaction sees each payload whole and only then cuts payloads
//! larger than [`MAX_RECORDED_PAYLOAD`] to a preview with `truncated: true`
//! (`size` keeps the full size), so a cut never leaves part of a secret.
//! Types and ids (an SSE event's, which the server picks) are cut the same
//! way to [`MAX_RECORDED_FIELD`] bytes.

use serde::{Deserialize, Serialize};

use super::record::HeaderRecord;
use crate::redact::Redactor;

/// The most bytes of a payload a record keeps (64 KiB).
pub const MAX_RECORDED_PAYLOAD: usize = 64 * 1024;

/// The most bytes of an entry's type or id a record keeps (1 KiB).
pub const MAX_RECORDED_FIELD: usize = 1024;

/// The realtime side of one executed `kind: ws` or `kind: sse` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RealtimeRecord {
    /// `ws` or `sse`.
    pub protocol: String,
    /// The server's answer to the opening request; absent when none came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handshake: Option<HandshakeRecord>,
    /// The kept entries, oldest first.
    pub timeline: Vec<TimelineRecord>,
    /// Entries dropped from the start of the timeline to stay under
    /// `max_timeline`.
    pub dropped_entries: u64,
    /// Messages or events received, dropped ones included.
    pub received: u64,
    /// How the session ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EndRecord>,
    pub elapsed_ms: f64,
}

/// The response to the opening request (the WebSocket upgrade, or the
/// SSE response head).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandshakeRecord {
    pub status: u16,
    pub headers: Vec<HeaderRecord>,
    /// The subprotocol the server chose (WebSocket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subprotocol: Option<String>,
    /// The body of a refused upgrade (at most 64 KiB once redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// One frame, event or system note.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineRecord {
    /// `sent`, `received` or `system`.
    pub dir: String,
    /// Milliseconds since the session started.
    pub t_ms: u64,
    /// The full payload size in bytes.
    pub size: u64,
    /// `text`, `binary`, `json`, `ping`, `pong`, `close`, a system note or
    /// the SSE event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Text; binary data in base64. At most [`MAX_RECORDED_PAYLOAD`] bytes
    /// once redacted.
    pub payload: String,
    /// The payload was cut to [`MAX_RECORDED_PAYLOAD`] bytes, or the type
    /// or id to [`MAX_RECORDED_FIELD`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The SSE event id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The round-trip time of a pong answering one of our pings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u64>,
}

impl TimelineRecord {
    /// An entry with its whole `payload`: [`RealtimeRecord::redacted`] cuts
    /// it to a preview after redacting it.
    pub fn new(dir: &str, t_ms: u64, size: u64, kind: &str, payload: &str) -> Self {
        Self {
            dir: dir.to_owned(),
            t_ms,
            size,
            kind: kind.to_owned(),
            payload: payload.to_owned(),
            truncated: false,
            id: None,
            rtt_ms: None,
        }
    }
}

/// How the session ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EndRecord {
    /// `closed by server: 1008 policy`, `max_messages reached (50)`...
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
}

impl RealtimeRecord {
    /// This record with every secret `redactor` knows replaced in its
    /// handshake, payloads, types, ids and end, and sensitive headers
    /// hidden. Payloads and the handshake body are redacted whole, then cut
    /// to [`MAX_RECORDED_PAYLOAD`] bytes; types and ids to
    /// [`MAX_RECORDED_FIELD`].
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |t: &str| redactor.text(t).into_owned();
        if let Some(handshake) = &mut self.handshake {
            for header in &mut handshake.headers {
                header.value = redactor.header_value(&header.name, &header.value);
            }
            handshake.body = handshake
                .body
                .as_deref()
                .map(|b| preview(&text(b), MAX_RECORDED_PAYLOAD).0);
        }
        for entry in &mut self.timeline {
            entry.redact(redactor);
        }
        if let Some(end) = &mut self.end {
            end.reason = text(&end.reason);
            end.close_reason = end.close_reason.as_deref().map(text);
        }
        self
    }
}

impl TimelineRecord {
    /// Redact this entry's payload, type and id whole, then cut them to
    /// their previews.
    fn redact(&mut self, redactor: &Redactor) {
        let (payload, cut) = preview(&redactor.text(&self.payload), MAX_RECORDED_PAYLOAD);
        let (kind, kind_cut) = preview(&redactor.text(&self.kind), MAX_RECORDED_FIELD);
        let id = self
            .id
            .as_deref()
            .map(|id| preview(&redactor.text(id), MAX_RECORDED_FIELD));
        self.truncated |= cut || kind_cut || id.as_ref().is_some_and(|(_, cut)| *cut);
        self.payload = payload;
        self.kind = kind;
        self.id = id.map(|(id, _)| id);
    }
}

/// `text`, cut at a character boundary to at most `max` bytes, and whether
/// it was cut.
fn preview(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    fn with_entries(timeline: Vec<TimelineRecord>, body: Option<String>) -> RealtimeRecord {
        RealtimeRecord {
            protocol: "sse".into(),
            handshake: Some(HandshakeRecord {
                status: 401,
                headers: Vec::new(),
                subprotocol: None,
                body,
            }),
            timeline,
            dropped_entries: 0,
            received: 1,
            end: None,
            elapsed_ms: 1.0,
        }
    }

    #[test]
    fn large_payloads_are_kept_as_previews() {
        let small = TimelineRecord::new("received", 1, 3, "text", "abc");
        let text = "é".repeat(MAX_RECORDED_PAYLOAD);
        let big = TimelineRecord::new("received", 1, text.len() as u64, "text", &text);
        let record = with_entries(vec![small, big], None).redacted(&Redactor::new());
        let (small, big) = (&record.timeline[0], &record.timeline[1]);
        assert_eq!(small.payload, "abc");
        assert!(!small.truncated);
        assert!(big.truncated);
        assert!(big.payload.len() <= MAX_RECORDED_PAYLOAD);
        assert_eq!(big.size, (2 * MAX_RECORDED_PAYLOAD) as u64);
        let shown = serde_json::to_value(small).unwrap();
        assert!(shown.get("truncated").is_none());
        assert_eq!(shown["type"], "text");
    }

    #[test]
    fn a_secret_across_the_preview_cut_is_redacted_whole() {
        let secret = "CANARYzq81wXyz";
        let redactor = Redactor::new().with(&Secret::new("api_token", secret.to_owned()));
        let text = format!("{}{secret}", "x".repeat(MAX_RECORDED_PAYLOAD - 8));
        let entry = TimelineRecord::new("received", 1, text.len() as u64, "message", &text);
        let record = with_entries(vec![entry], Some(text.clone())).redacted(&redactor);
        let payload = &record.timeline[0].payload;
        let body = record.handshake.unwrap().body.unwrap();
        for shown in [payload, &body] {
            assert!(shown.len() <= MAX_RECORDED_PAYLOAD, "{}", shown.len());
            assert!(
                !redactor.reveals_secret(shown),
                "{}",
                &shown[shown.len() - 20..]
            );
            assert!(!shown.contains("CANARY"));
        }
        assert!(record.timeline[0].truncated);
        assert_eq!(record.timeline[0].size, text.len() as u64);
    }

    #[test]
    fn long_types_and_ids_are_kept_as_previews() {
        let long = "k".repeat(MAX_RECORDED_FIELD + 10);
        let mut entry = TimelineRecord::new("received", 1, 1, &long, "a");
        entry.id = Some(long.clone());
        let short = TimelineRecord::new("received", 1, 1, "update", "b");
        let record = with_entries(vec![entry, short], None).redacted(&Redactor::new());
        let (cut, kept) = (&record.timeline[0], &record.timeline[1]);
        assert_eq!(cut.kind.len(), MAX_RECORDED_FIELD);
        assert_eq!(cut.id.as_ref().unwrap().len(), MAX_RECORDED_FIELD);
        assert_eq!(cut.payload, "a");
        assert!(cut.truncated);
        assert_eq!(kept.kind, "update");
        assert!(!kept.truncated);
    }

    #[test]
    fn payloads_headers_and_ends_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("api_token", "t0k".to_owned()));
        let mut entry = TimelineRecord::new("received", 1, 7, "type-t0k", "say t0k");
        entry.id = Some("t0k".to_owned());
        let record = RealtimeRecord {
            protocol: "ws".into(),
            handshake: Some(HandshakeRecord {
                status: 101,
                headers: vec![
                    HeaderRecord {
                        name: "Set-Cookie".into(),
                        value: "sid=1".into(),
                    },
                    HeaderRecord {
                        name: "X-Echo".into(),
                        value: "t0k".into(),
                    },
                ],
                subprotocol: None,
                body: None,
            }),
            timeline: vec![entry],
            dropped_entries: 0,
            received: 1,
            end: Some(EndRecord {
                reason: "closed by server: 1008 t0k".into(),
                close_code: Some(1008),
                close_reason: Some("t0k".into()),
            }),
            elapsed_ms: 1.0,
        };
        let shown = serde_json::to_string(&record.redacted(&redactor)).unwrap();
        assert!(!shown.contains("t0k"), "{shown}");
        assert!(!shown.contains("sid=1"), "{shown}");
        assert!(shown.contains("«redacted:api_token»"), "{shown}");
    }
}
