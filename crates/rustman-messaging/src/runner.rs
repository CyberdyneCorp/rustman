//! Running the messaging requests of a workspace under the `rustman-http`
//! runner: [`MessagingRunner`] plugs the clients into it, and the runner
//! resolves the request, learns its secrets, evaluates assertions and
//! records history.
//!
//! Under `rustman run` consumes are headless: they stop at their bounds.
//! Front ends can override the bounds (`--max-messages`, `--duration`),
//! cap them (agents), keep a consume open until the user stops it (the
//! TUI), follow it live (a feed of consumed messages) and gate it with a
//! permission (agents: produce needs `allow_mutations`).

use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::model::MessagingLimits;
use rustman_core::run::redact_value;
use rustman_core::safety::Permission;
use rustman_http::runner::{MessagingCall, MessagingExecutor, MessagingFuture, MessagingOutcome};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::asyncapi::schema_assertion;
use crate::{Bounds, ConsumedMessage, DEFAULT_CONNECT_TIMEOUT, RunOptions, run, safety};

/// Runs the messaging operations of a run.
#[derive(Debug, Default)]
pub struct MessagingRunner {
    /// `--max-messages`: replaces every consume's `limits.max_messages`.
    pub max_messages: Option<u32>,
    /// `--duration`: replaces every consume's `limits.duration_ms`.
    pub duration: Option<Duration>,
    /// The most any consume may run, whatever its file or the overrides
    /// say (agents).
    pub ceiling: Option<Bounds>,
    /// Interactive: consumes have no bounds and run until this token is
    /// cancelled.
    pub interactive: Option<CancellationToken>,
    /// Gets every consumed message as JSON as it is recorded, redacted
    /// with every secret of the run and the request.
    pub feed: Option<mpsc::UnboundedSender<Value>>,
    /// Agent paths: operations whose class this permission does not allow
    /// are refused before anything connects. `None` (a human) runs all.
    pub permission: Option<Permission>,
}

impl MessagingExecutor for MessagingRunner {
    fn execute<'a>(&'a self, call: MessagingCall<'a>) -> MessagingFuture<'a> {
        Box::pin(self.run(call))
    }
}

impl MessagingRunner {
    /// The headless bounds of a consume with `limits`, with this run's
    /// overrides, under the ceiling.
    pub fn bounds_of(&self, limits: &MessagingLimits) -> Bounds {
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

    /// Pass every message of `raw` to the feed, redacted, until the
    /// operation ends.
    async fn forward(&self, mut raw: mpsc::UnboundedReceiver<ConsumedMessage>, redactor: Redactor) {
        let Some(feed) = &self.feed else {
            return;
        };
        while let Some(message) = raw.recv().await {
            let value = serde_json::to_value(&message).unwrap_or(Value::Null);
            let _ = feed.send(redact_value(&value, &redactor));
        }
    }

    async fn run(&self, call: MessagingCall<'_>) -> MessagingOutcome {
        let resolved = call.resolved;
        if let Some(permission) = self.permission
            && let Err(refusal) = safety::check(permission, resolved)
        {
            return MessagingOutcome::blocked(refusal.to_string());
        }
        let (bounds, cancel) = match &self.interactive {
            Some(stop) => (None, stop.clone()),
            None => (Some(self.bounds_of(&resolved.limits)), call.cancel.clone()),
        };
        let (feed, raw) = mpsc::unbounded_channel();
        let options = RunOptions {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            bounds,
            cancel,
            ready: call.ready,
            feed: self.feed.is_some().then_some(feed),
        };
        let started = std::time::Instant::now();
        let forwarded = self.forward(raw, call.redactor.clone());
        let (result, ()) = tokio::join!(run(resolved, options), forwarded);
        match result {
            Ok(exchange) => MessagingOutcome {
                exchange: exchange.to_value(),
                error: None,
                blocked: false,
                implicit: schema_assertion(&exchange).into_iter().collect(),
                elapsed: Duration::from_millis(exchange.duration_ms),
            },
            Err(err) => MessagingOutcome {
                exchange: Value::Null,
                error: Some(err.to_string()),
                blocked: false,
                implicit: Vec::new(),
                elapsed: started.elapsed(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_the_file_and_the_ceiling_caps_both() {
        let limits = MessagingLimits {
            max_messages: Some(500),
            duration_ms: Some(120_000),
            ..MessagingLimits::default()
        };
        let plain = MessagingRunner::default().bounds_of(&limits);
        assert_eq!(plain.max_messages, 500);
        assert_eq!(plain.duration, Duration::from_secs(120));
        let overridden = MessagingRunner {
            max_messages: Some(3),
            duration: Some(Duration::from_secs(2)),
            ..MessagingRunner::default()
        };
        let bounds = overridden.bounds_of(&limits);
        assert_eq!(
            (bounds.max_messages, bounds.duration),
            (3, Duration::from_secs(2))
        );
        let capped = MessagingRunner {
            ceiling: Some(Bounds {
                max_messages: 100,
                duration: Duration::from_secs(60),
            }),
            ..MessagingRunner::default()
        };
        let bounds = capped.bounds_of(&limits);
        assert_eq!(
            (bounds.max_messages, bounds.duration),
            (100, Duration::from_secs(60))
        );
        let defaults = MessagingRunner::default().bounds_of(&MessagingLimits::default());
        assert_eq!(
            (defaults.max_messages, defaults.duration),
            (10, Duration::from_secs(30))
        );
    }
}
