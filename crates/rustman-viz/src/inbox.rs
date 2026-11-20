//! Messages from the reader thread to the UI thread (design D4): a bounded
//! queue of 1,024 messages. When it is full, a progress message
//! (`exchange_completed`, `probe_completed`) replaces the oldest progress
//! message still queued, so the newest progress wins; `finding`,
//! `run_finished` and `exploration_finished` are never dropped.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use rustman_viz_protocol::Message;

/// The default capacity.
pub const CAPACITY: usize = 1024;

pub struct Inbox {
    state: Mutex<State>,
    capacity: usize,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Message>,
    closed: bool,
    coalesced: u64,
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new(CAPACITY)
    }
}

impl Inbox {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            capacity: capacity.max(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue `message`, coalescing progress when full.
    pub fn push(&self, message: Message) {
        let mut state = self.lock();
        let full = state.queue.len() >= self.capacity && !message.is_priority();
        if let Some(oldest) = full
            .then(|| state.queue.iter().position(|m| !m.is_priority()))
            .flatten()
        {
            state.queue.remove(oldest);
            state.coalesced += 1;
        }
        state.queue.push_back(message);
    }

    /// Everything queued, oldest first.
    pub fn drain(&self) -> Vec<Message> {
        self.lock().queue.drain(..).collect()
    }

    /// The producer is gone (end of stdin).
    pub fn close(&self) {
        self.lock().closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// How many progress messages were replaced by newer ones.
    pub fn coalesced(&self) -> u64 {
        self.lock().coalesced
    }
}

#[cfg(test)]
mod tests {
    use rustman_viz_protocol::{
        Finding, Outcome, ProbeCompleted, RunFinished, RunState, RunSummary, Severity,
    };

    use super::*;

    fn probe(n: u64) -> Message {
        Message::ProbeCompleted(ProbeCompleted {
            operation: format!("op{n}"),
            outcome: Outcome::Passed,
            status: Some(200),
            duration_ms: n,
        })
    }

    fn finding(n: u64) -> Message {
        Message::Finding(Finding {
            severity: Severity::High,
            operation: format!("op{n}"),
            title: "t".into(),
            detail: None,
        })
    }

    #[test]
    fn a_full_inbox_keeps_the_newest_progress_and_every_finding() {
        let inbox = Inbox::new(4);
        for n in 0..4 {
            inbox.push(probe(n));
        }
        inbox.push(probe(4));
        inbox.push(finding(0));
        inbox.push(finding(1));
        let finished = Message::RunFinished(RunFinished {
            state: RunState::Completed,
            summary: RunSummary::default(),
        });
        inbox.push(finished.clone());
        // Over capacity now: more progress replaces the oldest progress.
        inbox.push(probe(5));
        inbox.push(probe(6));
        let drained = inbox.drain();
        assert_eq!(
            drained,
            [
                probe(3),
                probe(4),
                finding(0),
                finding(1),
                finished,
                probe(5),
                probe(6)
            ]
        );
        assert_eq!(inbox.coalesced(), 3);
    }

    #[test]
    fn a_burst_of_findings_is_never_dropped() {
        let inbox = Inbox::new(16);
        for n in 0..500 {
            inbox.push(finding(n));
            inbox.push(probe(n));
        }
        let drained = inbox.drain();
        let findings = drained
            .iter()
            .filter(|m| matches!(m, Message::Finding(_)))
            .count();
        assert_eq!(findings, 500);
    }
}
