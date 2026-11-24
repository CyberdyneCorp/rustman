//! A finished session as the runner records and checks it: the history
//! record (design D5, payloads above 64 KiB cut to a preview once
//! redacted), the received
//! messages assertions read at `$.messages[*]`, and the session's facts.

use rustman_core::assertions::RealtimeFacts;
use rustman_core::run::{EndRecord, HandshakeRecord, HeaderRecord, RealtimeRecord, TimelineRecord};
use rustman_http::millis;
use rustman_http::runner::RealtimeOutcome;
use serde_json::Value;

use crate::exchange::{Handshake, RealtimeExchange};
use crate::timeline::TimelineEntry;

/// The runner's view of `exchange`.
pub fn outcome(exchange: &RealtimeExchange) -> RealtimeOutcome {
    let handshake = exchange.handshake.as_ref();
    RealtimeOutcome {
        request: exchange.request.clone(),
        error: exchange.error.as_ref().map(ToString::to_string),
        record: record(exchange),
        messages: messages(exchange),
        status: handshake.map_or(0, |h| h.status),
        headers: handshake.map(|h| h.headers.clone()).unwrap_or_default(),
        first_message: exchange
            .first_message_ms
            .map(std::time::Duration::from_millis),
        facts: RealtimeFacts {
            received: exchange.received,
            close_code: exchange.close_code(),
        },
        elapsed: exchange.elapsed,
        credentials: exchange.credentials.clone(),
    }
}

/// The history record of `exchange`, not yet redacted.
pub fn record(exchange: &RealtimeExchange) -> RealtimeRecord {
    RealtimeRecord {
        protocol: exchange.protocol.to_owned(),
        handshake: exchange.handshake.as_ref().map(handshake),
        timeline: exchange.timeline.entries().map(entry).collect(),
        dropped_entries: exchange.timeline.dropped(),
        received: exchange.received,
        end: exchange.end.as_ref().map(|end| EndRecord {
            reason: end.reason.clone(),
            close_code: end.close_code,
            close_reason: end.close_reason.clone(),
        }),
        elapsed_ms: millis(exchange.elapsed),
    }
}

/// The received messages of `exchange`, each parsed as JSON or kept as
/// text.
pub fn messages(exchange: &RealtimeExchange) -> Vec<Value> {
    exchange
        .timeline
        .entries()
        .filter(|e| e.is_message())
        .map(|e| {
            serde_json::from_str(&e.payload).unwrap_or_else(|_| Value::String(e.payload.clone()))
        })
        .collect()
}

fn handshake(h: &Handshake) -> HandshakeRecord {
    HandshakeRecord {
        status: h.status,
        headers: h
            .headers
            .iter()
            .map(|(name, value)| HeaderRecord {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        subprotocol: h.subprotocol.clone(),
        body: h.body.clone(),
    }
}

fn entry(e: &TimelineEntry) -> TimelineRecord {
    let mut recorded = TimelineRecord::new(e.dir.as_str(), e.t_ms, e.size, &e.kind, &e.payload);
    recorded.id = e.id.as_deref().map(str::to_owned);
    recorded.rtt_ms = e.rtt_ms;
    recorded
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rustman_core::model::DEFAULT_MAX_TIMELINE;
    use rustman_core::redact::Redactor;
    use rustman_core::run::MAX_RECORDED_PAYLOAD;
    use rustman_core::secret::Secret;
    use rustman_core::vars::ResolvedRequest;

    use super::*;
    use crate::timeline::Direction;

    fn exchange(max_timeline: u32) -> RealtimeExchange {
        RealtimeExchange::new("ws", empty_request(), Instant::now(), max_timeline)
    }

    fn empty_request() -> ResolvedRequest {
        use rustman_core::model::RequestSettings;
        use rustman_core::vars::{Resolved, ResolvedBody, TlsFiles};
        ResolvedRequest {
            method: "GET".into(),
            url: Resolved::literal("ws://h/"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        }
    }

    #[test]
    fn the_timeline_keeps_max_timeline_entries_and_counts_the_dropped() {
        let mut exchange = exchange(DEFAULT_MAX_TIMELINE);
        for n in 0..10_001 {
            exchange
                .timeline
                .record(Direction::Received, "text", 1, n.to_string());
            exchange.received_one();
        }
        let recorded = record(&exchange);
        assert_eq!(recorded.timeline.len(), 10_000);
        assert_eq!(recorded.dropped_entries, 1);
        assert_eq!(recorded.received, 10_001);
        assert_eq!(recorded.timeline[0].payload, "1");
        let shown = serde_json::to_value(&recorded).unwrap();
        assert_eq!(shown["dropped_entries"], 1);
    }

    #[test]
    fn messages_are_json_or_text_without_control_frames() {
        let mut exchange = exchange(10);
        let timeline = &mut exchange.timeline;
        timeline.record(
            Direction::Received,
            "json",
            17,
            r#"{"type":"snapshot"}"#.into(),
        );
        timeline.record(Direction::Received, "ping", 0, String::new());
        timeline.record(Direction::Sent, "text", 2, "hi".into());
        timeline.record(Direction::Received, "text", 5, "plain".into());
        let messages = messages(&exchange);
        assert_eq!(
            messages,
            [
                serde_json::json!({"type": "snapshot"}),
                Value::String("plain".into())
            ]
        );
    }

    #[test]
    fn large_payloads_are_recorded_as_previews() {
        let mut exchange = exchange(10);
        let big = "x".repeat(MAX_RECORDED_PAYLOAD + 1);
        let size = big.len();
        exchange
            .timeline
            .record(Direction::Received, "text", size, big);
        let recorded = record(&exchange).redacted(&Redactor::new());
        assert!(recorded.timeline[0].truncated);
        assert_eq!(recorded.timeline[0].size, size as u64);
        assert_eq!(recorded.timeline[0].payload.len(), MAX_RECORDED_PAYLOAD);
        assert_eq!(messages(&exchange)[0].as_str().unwrap().len(), size);
    }

    #[test]
    fn a_secret_at_the_preview_boundary_never_reaches_the_record() {
        let secret = "CANARYzq81wXyz";
        let redactor = Redactor::new().with(&Secret::new("api_token", secret.to_owned()));
        let mut exchange = exchange(10);
        let text = format!("{}{secret}", "x".repeat(MAX_RECORDED_PAYLOAD - 8));
        let size = text.len();
        exchange
            .timeline
            .record(Direction::Received, "message", size, text.clone());
        exchange.handshake = Some(Handshake {
            status: 401,
            headers: Vec::new(),
            subprotocol: None,
            body: Some(text),
        });
        let shown = serde_json::to_string(&record(&exchange).redacted(&redactor)).unwrap();
        assert!(!shown.contains("CANARY"));
        assert!(shown.contains("x«redac"));
    }
}
