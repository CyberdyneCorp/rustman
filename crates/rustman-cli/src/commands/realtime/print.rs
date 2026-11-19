//! Printing received messages as they arrive. Entries come from the
//! session's live feed, which redacts every secret of the request before
//! sending them, so no secret reaches the terminal.

use std::io::Write;

use rustman_realtime::{TimelineEntry, message_json};

/// How messages are printed: each payload as received, newlines included
/// (a multi-line SSE event spans several lines), or one JSON object per
/// message on one line (`--json`).
#[derive(Clone, Copy, Debug)]
pub struct Printer {
    json: bool,
}

impl Printer {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    /// The line `entry` prints as; `None` for control frames, sent frames
    /// and system notes.
    pub fn line(self, entry: &TimelineEntry) -> Option<String> {
        if !entry.is_message() {
            return None;
        }
        Some(if self.json {
            message_json(entry).to_string()
        } else {
            entry.payload.clone()
        })
    }

    /// Print `entry`, if it is a message. A closed stdout (`| head`) is
    /// not worth an error: the session still ends at its bounds.
    pub fn entry(self, entry: &TimelineEntry) {
        if let Some(line) = self.line(entry) {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}").and_then(|()| out.flush());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_realtime::Direction;

    fn entry(dir: Direction, kind: &str, payload: &str) -> TimelineEntry {
        TimelineEntry {
            dir,
            t_ms: 12,
            size: payload.len() as u64,
            kind: kind.to_owned(),
            payload: payload.to_owned(),
            id: Some("42".to_owned()),
            rtt_ms: None,
        }
    }

    #[test]
    fn only_received_messages_are_printed() {
        let plain = Printer::new(false);
        let received = entry(Direction::Received, "price", "{\"p\":1}");
        assert_eq!(plain.line(&received).as_deref(), Some("{\"p\":1}"));
        assert_eq!(plain.line(&entry(Direction::Sent, "text", "a")), None);
        assert_eq!(plain.line(&entry(Direction::Received, "ping", "")), None);
        assert_eq!(
            plain.line(&entry(Direction::System, "connect", "connected")),
            None
        );
        let json = Printer::new(true).line(&received).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "price");
        assert_eq!(value["id"], "42");
        assert_eq!(value["data"], "{\"p\":1}");
        assert_eq!(value["t_ms"], 12);
    }

    #[test]
    fn multi_line_data_spans_lines_unless_printed_as_json() {
        let event = entry(Direction::Received, "update", "line1\nline2");
        let plain = Printer::new(false).line(&event).unwrap();
        assert_eq!(plain, "line1\nline2", "the payload as received");
        let json = Printer::new(true).line(&event).unwrap();
        assert!(!json.contains('\n'), "{json}");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["data"], "line1\nline2");
    }
}
