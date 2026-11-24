//! The message timeline of a session (design D5): one entry per frame or
//! event with its direction, time since the session started, size, type
//! and payload. At most `limits.max_timeline` entries, holding at most
//! [`MAX_TIMELINE_BYTES`] (payloads, types and ids counted), are kept; the
//! oldest are dropped and counted.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use rustman_core::Redactor;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

/// The most bytes a timeline keeps (256 MiB) of payloads, types and ids:
/// past it, the oldest entries are dropped, so memory stays bounded
/// however large the messages are. The newest entry is always kept.
pub const MAX_TIMELINE_BYTES: usize = 256 * 1024 * 1024;

/// Timeline types that carry no message: control frames.
const CONTROL: [&str; 3] = ["ping", "pong", "close"];

/// Whether an entry going `dir` with type `kind` is a received message
/// (a frame or event with data, not a control frame or a system note).
pub fn is_message(dir: &str, kind: &str) -> bool {
    dir == Direction::Received.as_str() && !CONTROL.contains(&kind)
}

/// A message as one JSON object: `type`, `id` (`null` without one),
/// `data` and `t_ms`, with `dir` and `size`.
pub fn message_json(entry: &TimelineEntry) -> Value {
    json!({
        "t_ms": entry.t_ms,
        "dir": entry.dir.as_str(),
        "type": entry.kind,
        "size": entry.size,
        "id": entry.id.as_deref(),
        "data": entry.payload,
    })
}

/// Where an entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Sent,
    Received,
    /// Connect, close and reconnect.
    System,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Received => "received",
            Self::System => "system",
        }
    }
}

/// One frame, event or system note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TimelineEntry {
    pub dir: Direction,
    /// Milliseconds since the session started.
    pub t_ms: u64,
    /// The payload size in bytes.
    pub size: u64,
    /// `text`, `binary`, `json`, `ping`, `pong`, `close`, a system note
    /// (`connect`, `close`) or the SSE event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Text as received; binary data in base64.
    pub payload: String,
    /// The SSE event id (the last event id when the event arrived), shared
    /// with the other events that carry it rather than copied into each.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_id"
    )]
    pub id: Option<Arc<str>>,
    /// The round-trip time of a pong answering one of our pings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u64>,
}

impl TimelineEntry {
    /// Whether this is a received message rather than a control frame or
    /// a system note.
    pub fn is_message(&self) -> bool {
        is_message(self.dir.as_str(), &self.kind)
    }

    /// The bytes this entry holds that the server controls: its payload,
    /// type and id, as the timeline's byte budget counts them. A shared id
    /// is counted for each entry, so the budget never undercounts.
    pub fn held_bytes(&self) -> usize {
        self.payload.len() + self.kind.len() + self.id.as_deref().map_or(0, str::len)
    }

    /// The entry with every secret `redactor` knows replaced in what the
    /// server controls: its payload, type (the SSE event name) and id.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |t: &str| redactor.text(t).into_owned();
        self.payload = text(&self.payload);
        self.kind = text(&self.kind);
        // Kept shared unless it held a secret.
        self.id = self.id.map(|id| match redactor.text(&id) {
            Cow::Borrowed(_) => id,
            Cow::Owned(redacted) => Arc::from(redacted),
        });
        self
    }
}

/// An id as the string it holds.
fn serialize_id<S: Serializer>(id: &Option<Arc<str>>, serializer: S) -> Result<S::Ok, S::Error> {
    id.as_deref().serialize(serializer)
}

/// The entries of a session, capped.
#[derive(Clone, Debug)]
pub struct Timeline {
    started: Instant,
    entries: VecDeque<TimelineEntry>,
    max: usize,
    /// The bytes kept ([`TimelineEntry::held_bytes`]), and the most that
    /// may be.
    bytes: usize,
    max_bytes: usize,
    dropped: u64,
}

impl Timeline {
    /// An empty timeline that keeps at most `max` entries (at least 1) and
    /// [`MAX_TIMELINE_BYTES`].
    pub fn new(started: Instant, max: u32) -> Self {
        Self {
            started,
            entries: VecDeque::new(),
            max: usize::try_from(max.max(1)).unwrap_or(usize::MAX),
            bytes: 0,
            max_bytes: MAX_TIMELINE_BYTES,
            dropped: 0,
        }
    }

    /// This timeline keeping at most `max_bytes`.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Milliseconds since the session started.
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Record an entry stamped now; returns it.
    pub fn record(
        &mut self,
        dir: Direction,
        kind: &str,
        size: usize,
        payload: String,
    ) -> &TimelineEntry {
        let entry = TimelineEntry {
            dir,
            t_ms: self.now_ms(),
            size: size as u64,
            kind: kind.to_owned(),
            payload,
            id: None,
            rtt_ms: None,
        };
        self.push(entry)
    }

    /// Append `entry`, dropping the oldest while the timeline is full or
    /// would hold too many bytes.
    pub fn push(&mut self, entry: TimelineEntry) -> &TimelineEntry {
        let size = entry.held_bytes();
        while !self.entries.is_empty()
            && (self.entries.len() >= self.max || self.bytes + size > self.max_bytes)
        {
            if let Some(oldest) = self.entries.pop_front() {
                self.bytes -= oldest.held_bytes();
                self.dropped += 1;
            }
        }
        self.bytes += size;
        self.entries.push_back(entry);
        self.entries.back().expect("just pushed")
    }

    pub fn entries(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The kept entry at `index`, oldest first.
    pub fn get(&self, index: usize) -> Option<&TimelineEntry> {
        self.entries.get(index)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries were dropped to stay under the cap.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The entries, oldest first.
    pub fn to_vec(&self) -> Vec<TimelineEntry> {
        self.entries.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_entries_are_dropped_and_counted() {
        let mut timeline = Timeline::new(Instant::now(), 3);
        for n in 0..5 {
            timeline.record(Direction::Received, "text", 1, n.to_string());
        }
        let kept: Vec<&str> = timeline.entries().map(|e| e.payload.as_str()).collect();
        assert_eq!(kept, ["2", "3", "4"]);
        assert_eq!(timeline.dropped(), 2);
    }

    #[test]
    fn the_oldest_entries_are_dropped_past_the_byte_budget() {
        let mut timeline = Timeline::new(Instant::now(), 100).with_max_bytes(10);
        for payload in ["aaaa", "bbbb", "cccc", "dddddddddddd"] {
            timeline.record(Direction::Received, "text", payload.len(), payload.into());
            assert!(timeline.len() == 1 || timeline.bytes <= 10);
        }
        let kept: Vec<&str> = timeline.entries().map(|e| e.payload.as_str()).collect();
        assert_eq!(kept, ["dddddddddddd"], "the newest entry is always kept");
        assert_eq!(timeline.dropped(), 3);
        timeline.record(Direction::Received, "text", 2, "ee".into());
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline.dropped(), 4);
    }

    #[test]
    fn event_types_and_ids_count_toward_the_byte_budget() {
        let mut timeline = Timeline::new(Instant::now(), 100).with_max_bytes(20);
        for n in 0..3_u64 {
            let entry = TimelineEntry {
                dir: Direction::Received,
                t_ms: n,
                size: 1,
                kind: "k".repeat(8),
                payload: n.to_string(),
                id: Some("i".repeat(8).into()),
                rtt_ms: None,
            };
            timeline.push(entry);
            assert!(timeline.bytes <= 20, "{}", timeline.bytes);
        }
        assert_eq!(timeline.len(), 1, "each entry holds 17 bytes");
        assert_eq!(timeline.dropped(), 2);
    }

    #[test]
    fn entries_serialize_with_the_spec_field_names() {
        let mut timeline = Timeline::new(Instant::now(), 10);
        timeline.record(Direction::Sent, "json", 2, "{}".into());
        let value = serde_json::to_value(timeline.to_vec()).unwrap();
        let entry = &value[0];
        assert_eq!(entry["dir"], "sent");
        assert_eq!(entry["type"], "json");
        assert_eq!(entry["size"], 2);
        assert!(entry["t_ms"].is_u64());
        assert!(entry.get("rtt_ms").is_none());
    }

    #[test]
    fn redaction_covers_the_payload_type_and_id() {
        let secret = rustman_core::Secret::new("token", "SECRETTOKEN0a1b".to_owned());
        let redactor = Redactor::new().with(&secret);
        let mut timeline = Timeline::new(Instant::now(), 10);
        let mut entry = timeline
            .record(
                Direction::Received,
                "t-SECRETTOKEN0a1b",
                3,
                "d-SECRETTOKEN0a1b".into(),
            )
            .clone();
        entry.id = Some("SECRETTOKEN0a1b".into());
        let shown = serde_json::to_string(&entry.redacted(&redactor)).unwrap();
        assert!(!shown.contains("SECRETTOKEN0a1b"), "{shown}");
        assert!(shown.contains("t-«redacted:token»"), "{shown}");
    }

    /// #82: redacting an entry keeps its id shared unless the id held a
    /// secret; serialized, the id is a plain string.
    #[test]
    fn redaction_keeps_an_id_without_secrets_shared() {
        let secret = rustman_core::Secret::new("token", "SECRETTOKEN0a1b".to_owned());
        let redactor = Redactor::new().with(&secret);
        let id: Arc<str> = Arc::from("evt-41");
        let mut timeline = Timeline::new(Instant::now(), 10);
        let mut entry = timeline
            .record(Direction::Received, "tick", 1, "x".into())
            .clone();
        entry.id = Some(Arc::clone(&id));
        let redacted = entry.redacted(&redactor);
        assert!(Arc::ptr_eq(redacted.id.as_ref().unwrap(), &id));
        assert_eq!(serde_json::to_value(&redacted).unwrap()["id"], "evt-41");
        assert_eq!(message_json(&redacted)["id"], "evt-41");
    }

    #[test]
    fn only_received_data_entries_are_messages() {
        let mut timeline = Timeline::new(Instant::now(), 10);
        timeline.record(Direction::System, "connect", 0, "connected".into());
        timeline.record(Direction::Sent, "text", 1, "a".into());
        timeline.record(Direction::Received, "pong", 0, String::new());
        timeline.record(Direction::Received, "update", 2, "{}".into());
        let messages: Vec<&TimelineEntry> = timeline.entries().filter(|e| e.is_message()).collect();
        assert_eq!(messages.len(), 1);
        let line = message_json(messages[0]);
        assert_eq!(line["type"], "update");
        assert_eq!(line["data"], "{}");
        assert!(line["id"].is_null());
        assert!(line["t_ms"].is_u64());
    }
}
