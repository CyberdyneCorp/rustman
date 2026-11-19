//! Running the realtime requests of a workspace under the `rustman-http`
//! runner: [`RealtimeRunner`] plugs the sessions into it, and the runner
//! resolves the request, learns its secrets and records history.
//!
//! Under `rustman run` sessions are headless: they stop at their bounds.
//! Front ends can also follow a session live (a feed of redacted timeline
//! entries), send frames while it runs (a command channel), cap its bounds
//! (agents) or keep it open until the user closes it (the TUI).

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustman_core::model::RealtimeLimits;
use rustman_core::vars::ResolvedRealtime;
use rustman_http::runner::{RealtimeCall, RealtimeExecutor, RealtimeFuture, RealtimeOutcome};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::record::outcome;
use crate::session::{Bounds, Command, SessionOptions};
use crate::sse::run_sse;
use crate::timeline::TimelineEntry;
use crate::ws::run_ws;

/// Runs the realtime sessions of a run.
#[derive(Debug, Default)]
pub struct RealtimeRunner {
    /// `--max-messages`: replaces every session's `limits.max_messages`.
    pub max_messages: Option<u32>,
    /// `--duration`: replaces every session's `limits.duration_ms`.
    pub duration: Option<Duration>,
    /// The most any session may run, whatever its file or the overrides
    /// say (agents: 100 messages, 60 s).
    pub ceiling: Option<Bounds>,
    /// Interactive: sessions have no bounds and stay open until this token
    /// is cancelled (or the server closes).
    pub interactive: Option<CancellationToken>,
    /// Gets every timeline entry, redacted, as it is recorded.
    pub feed: Option<mpsc::UnboundedSender<TimelineEntry>>,
    /// Frames and closes for the first session that starts (it takes the
    /// receiver).
    pub commands: Mutex<Option<mpsc::Receiver<Command>>>,
}

impl RealtimeExecutor for RealtimeRunner {
    fn execute<'a>(&'a self, call: RealtimeCall<'a>) -> RealtimeFuture<'a> {
        Box::pin(async move { Ok(self.run(&call).await) })
    }
}

impl RealtimeRunner {
    /// The headless bounds of `target`, with this run's overrides, under
    /// the ceiling.
    pub fn bounds(&self, target: &ResolvedRealtime) -> Bounds {
        self.bounds_of(target.limits())
    }

    /// The headless bounds of a session with `limits`.
    pub fn bounds_of(&self, limits: &RealtimeLimits) -> Bounds {
        let own = Bounds::of(limits);
        let wanted = Bounds {
            max_messages: self.max_messages.map_or(own.max_messages, u64::from),
            duration: self.duration.unwrap_or(own.duration),
        };
        match self.ceiling {
            Some(ceiling) => Bounds {
                max_messages: wanted.max_messages.min(ceiling.max_messages),
                duration: wanted.duration.min(ceiling.duration),
            },
            None => wanted,
        }
    }

    /// How the session of `call` runs.
    fn session(&self, call: &RealtimeCall<'_>) -> SessionOptions {
        let commands = self
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let (bounds, cancel) = match &self.interactive {
            Some(stop) => (None, stop.clone()),
            None => (Some(self.bounds(call.resolved)), call.cancel.clone()),
        };
        SessionOptions {
            bounds,
            cancel,
            feed: self.feed.clone(),
            commands,
        }
    }

    async fn run(&self, call: &RealtimeCall<'_>) -> RealtimeOutcome {
        let session = self.session(call);
        match call.resolved {
            ResolvedRealtime::Ws(ws) => outcome(&run_ws(ws, call.options, session).await),
            ResolvedRealtime::Sse(sse) => {
                outcome(&run_sse(call.engine, sse, call.options, session).await)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_the_file_and_the_ceiling_caps_both() {
        let limits = RealtimeLimits {
            max_messages: Some(500),
            duration_ms: Some(120_000),
            ..RealtimeLimits::default()
        };
        let plain = RealtimeRunner::default().bounds_of(&limits);
        assert_eq!(plain.max_messages, 500);
        assert_eq!(plain.duration, Duration::from_secs(120));
        let overridden = RealtimeRunner {
            max_messages: Some(3),
            ..RealtimeRunner::default()
        };
        assert_eq!(overridden.bounds_of(&limits).max_messages, 3);
        let capped = RealtimeRunner {
            ceiling: Some(Bounds {
                max_messages: 100,
                duration: Duration::from_secs(60),
            }),
            ..RealtimeRunner::default()
        };
        let bounds = capped.bounds_of(&limits);
        assert_eq!(bounds.max_messages, 100);
        assert_eq!(bounds.duration, Duration::from_secs(60));
        let small = RealtimeLimits {
            max_messages: Some(2),
            ..RealtimeLimits::default()
        };
        assert_eq!(capped.bounds_of(&small).max_messages, 2);
    }
}
