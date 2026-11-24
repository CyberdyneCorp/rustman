//! Subscriptions (add-graphql-client tasks 5.1 to 5.4): over WebSocket
//! (`graphql-transport-ws`, falling back to the legacy `graphql-ws`) and
//! over Server-Sent Events (distinct connections mode).
//!
//! Each result the server pushes becomes a timestamped
//! [`SubscriptionEvent`]. Collection stops when the server completes, when
//! the caller cancels, or, headless (`rustman run`, the CLI), at the
//! [`Limits`]: `max_events` events or `duration`, whichever comes first.
//! Interactive subscriptions have no limits and run until stopped; they
//! keep the first [`INTERACTIVE_KEPT_EVENTS`] events and count the rest.
//!
//! A subscription the server ends without `complete` (a WebSocket close,
//! with its close code, or the end of an SSE stream), or whose event
//! breaks the size limit, records why as its `failure`: an error of its
//! result, so `no_errors` fails and the CLI exits non-zero.

mod protocol;
mod sse;
mod ws;

use std::fmt;
use std::time::{Duration, Instant};

use http::HeaderMap;
use rustman_core::model::GraphqlRequest;
use rustman_core::run::now_rfc3339;
use rustman_core::trace::SentTrace;
use rustman_core::vars::ResolvedRequest;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::response::ResultStatus;

pub use protocol::WireProtocol;
pub use sse::subscribe_sse;
pub use ws::subscribe_ws;

/// How long a WebSocket server has to acknowledge `connection_init`.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// How many events an interactive subscription keeps for its result; later
/// ones are counted and fed to live views only.
pub const INTERACTIVE_KEPT_EVENTS: usize = 1000;

/// When a subscription stops on its own; `None` never stops it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub max_events: Option<u32>,
    pub duration: Option<Duration>,
}

impl Limits {
    /// Interactive: until stopped or completed.
    pub fn interactive() -> Self {
        Self::default()
    }

    /// Headless: the overrides (`--max-events`, `--duration`), else the
    /// request's `max_events` and `duration`, else 10 events and 30 s.
    pub fn headless(
        graphql: &GraphqlRequest,
        max_events: Option<u32>,
        duration: Option<Duration>,
    ) -> Self {
        Self {
            max_events: Some(max_events.unwrap_or_else(|| graphql.effective_max_events())),
            duration: Some(duration.unwrap_or_else(|| graphql.effective_duration())),
        }
    }
}

/// How a subscription runs.
#[derive(Clone, Debug)]
pub struct SubscribeOptions {
    pub limits: Limits,
    /// Stops collecting (the user's stop, or Ctrl+C).
    pub cancel: CancellationToken,
    /// How long a WebSocket server has to send `connection_ack`.
    pub ack_timeout: Duration,
    /// Receives every event as it arrives, for live views.
    pub feed: Option<UnboundedSender<SubscriptionEvent>>,
}

impl SubscribeOptions {
    pub fn new(limits: Limits, cancel: CancellationToken) -> Self {
        Self {
            limits,
            cancel,
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            feed: None,
        }
    }

    /// `limit`, shortened to what is left of the duration since `started`:
    /// a headless subscription waits for nothing past its duration.
    pub(crate) fn within(&self, started: Instant, limit: Duration) -> Duration {
        match self.limits.duration {
            Some(duration) => limit.min(duration.saturating_sub(started.elapsed())),
            None => limit,
        }
    }
}

/// One result the server pushed.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionEvent {
    /// When it arrived, RFC 3339 UTC.
    pub ts: String,
    /// Since the subscription started.
    pub elapsed: Duration,
    /// The execution result: `data`, `errors`, `extensions`.
    pub payload: Value,
}

/// Why collection stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The server sent `complete`.
    Complete,
    /// `max_events` events arrived.
    MaxEvents,
    /// `duration` elapsed.
    Duration,
    /// The caller stopped it.
    Stopped,
    /// The server closed the connection without `complete`; the
    /// subscription's `failure` says how.
    Closed,
    /// The server sent an `error` message (the last event), or an SSE
    /// event was larger than the limit (the `failure`).
    Error,
}

impl Ended {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::MaxEvents => "max_events",
            Self::Duration => "duration",
            Self::Stopped => "stopped",
            Self::Closed => "closed",
            Self::Error => "error",
        }
    }

    /// Whether the client ended it, and so tells the server to stop.
    fn by_client(self) -> bool {
        matches!(self, Self::MaxEvents | Self::Duration | Self::Stopped)
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The transport and protocol a subscription used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionProtocol {
    Ws(WireProtocol),
    Sse,
}

impl SubscriptionProtocol {
    /// As shown to users: `graphql-transport-ws`, `legacy
    /// subscriptions-transport-ws` or `sse`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ws(wire) => wire.label(),
            Self::Sse => "sse",
        }
    }
}

/// A finished subscription.
#[derive(Clone, Debug)]
pub struct Subscription {
    pub protocol: SubscriptionProtocol,
    /// The request that opened it: the WebSocket handshake (a `GET` of the
    /// `ws://` or `wss://` URL) or the SSE `POST`.
    pub request: ResolvedRequest,
    /// The handshake's status (`101`) or the stream's (`200`).
    pub status: u16,
    pub headers: HeaderMap,
    /// The events kept: all of them headless, the first
    /// [`INTERACTIVE_KEPT_EVENTS`] interactive.
    pub events: Vec<SubscriptionEvent>,
    /// Every event received, kept or not.
    pub event_count: usize,
    pub ended: Ended,
    /// Why the subscription failed when it did not end cleanly: the server
    /// closed it without `complete`, or an event was too large.
    pub failure: Option<String>,
    /// Until the connection was open.
    pub connected: Duration,
    /// Until collection stopped.
    pub elapsed: Duration,
    /// Over TLS with certificate verification off.
    pub insecure: bool,
    pub warnings: Vec<String>,
    /// The trace context the opening request carried (the WebSocket
    /// handshake or the SSE request): every event shares it.
    pub trace: Option<SentTrace>,
}

impl Subscription {
    /// The events as one result, which assertions read: `data` is the list
    /// of every event's data, `errors` every event's errors.
    pub fn result(&self) -> Value {
        let data: Vec<Value> = self
            .events
            .iter()
            .map(|e| e.payload.get("data").cloned().unwrap_or(Value::Null))
            .collect();
        let errors: Vec<Value> = self
            .events
            .iter()
            .filter_map(|e| e.payload.get("errors").and_then(Value::as_array))
            .flatten()
            .cloned()
            .chain(self.failure.iter().map(|f| json!({"message": f})))
            .collect();
        json!({"data": data, "errors": errors})
    }

    /// `ok` without errors, `partial` with errors and events with data,
    /// `errors` otherwise.
    pub fn status(&self) -> ResultStatus {
        let errors = self.failure.is_some()
            || self.events.iter().any(|e| {
                e.payload
                    .get("errors")
                    .and_then(Value::as_array)
                    .is_some_and(|errors| !errors.is_empty())
            });
        let data = self
            .events
            .iter()
            .any(|e| e.payload.get("data").is_some_and(|d| !d.is_null()));
        match (errors, data) {
            (false, _) => ResultStatus::Ok,
            (true, true) => ResultStatus::Partial,
            (true, false) => ResultStatus::Errors,
        }
    }
}

/// What a [`Collector`] gathered.
pub(crate) struct Collected {
    pub events: Vec<SubscriptionEvent>,
    pub event_count: usize,
    pub failure: Option<String>,
}

/// Gathers events within the limits.
pub(crate) struct Collector<'a> {
    options: &'a SubscribeOptions,
    started: Instant,
    events: Vec<SubscriptionEvent>,
    count: usize,
    failure: Option<String>,
}

impl<'a> Collector<'a> {
    pub fn new(options: &'a SubscribeOptions, started: Instant) -> Self {
        Self {
            options,
            started,
            events: Vec::new(),
            count: 0,
            failure: None,
        }
    }

    /// Record `payload`; `true` once `max_events` events arrived.
    pub fn push(&mut self, payload: Value) -> bool {
        let event = SubscriptionEvent {
            ts: now_rfc3339(),
            elapsed: self.started.elapsed(),
            payload,
        };
        self.count += 1;
        if let Some(feed) = &self.options.feed {
            // A live view that went away does not stop the subscription.
            let _ = feed.send(event.clone());
        }
        let bounded = self.options.limits.max_events.is_some();
        if bounded || self.count <= INTERACTIVE_KEPT_EVENTS {
            self.events.push(event);
        }
        self.options
            .limits
            .max_events
            .is_some_and(|max| self.count >= max as usize)
    }

    /// Record why the subscription failed.
    pub fn fail(&mut self, failure: String) {
        self.failure = Some(failure);
    }

    /// Sleeps until `duration` elapsed; forever without one.
    pub async fn deadline(&self) {
        match self.options.limits.duration {
            Some(duration) => tokio::time::sleep_until((self.started + duration).into()).await,
            None => std::future::pending().await,
        }
    }

    pub fn finish(self) -> Collected {
        Collected {
            events: self.events,
            event_count: self.count,
            failure: self.failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(payload: Value) -> SubscriptionEvent {
        SubscriptionEvent {
            ts: String::new(),
            elapsed: Duration::ZERO,
            payload,
        }
    }

    fn subscription(events: Vec<SubscriptionEvent>) -> Subscription {
        Subscription {
            protocol: SubscriptionProtocol::Sse,
            request: ResolvedRequest {
                method: "POST".into(),
                url: rustman_core::vars::Resolved::literal("http://h/graphql"),
                query: Vec::new(),
                headers: Vec::new(),
                body: rustman_core::vars::ResolvedBody::None,
                auth: None,
                settings: Default::default(),
                tls: Default::default(),
                secrets: Vec::new(),
            },
            status: 200,
            headers: HeaderMap::new(),
            event_count: events.len(),
            events,
            ended: Ended::Complete,
            failure: None,
            connected: Duration::ZERO,
            elapsed: Duration::ZERO,
            insecure: false,
            warnings: Vec::new(),
            trace: None,
        }
    }

    #[test]
    fn events_combine_into_one_result() {
        let sub = subscription(vec![
            event(json!({"data": {"n": 1}})),
            event(json!({"data": null, "errors": [{"message": "boom"}]})),
        ]);
        assert_eq!(
            sub.result(),
            json!({"data": [{"n": 1}, null], "errors": [{"message": "boom"}]})
        );
        assert_eq!(sub.status(), ResultStatus::Partial);
        assert_eq!(subscription(Vec::new()).status(), ResultStatus::Ok);
    }

    #[test]
    fn a_failure_is_an_error_of_the_result() {
        let mut sub = subscription(vec![event(json!({"data": {"n": 1}}))]);
        sub.failure = Some("closed: 4403 Forbidden".into());
        assert_eq!(
            sub.result()["errors"],
            json!([{"message": "closed: 4403 Forbidden"}])
        );
        assert_eq!(sub.status(), ResultStatus::Partial);
        sub.events.clear();
        assert_eq!(sub.status(), ResultStatus::Errors);
    }

    #[test]
    fn interactive_subscriptions_keep_the_first_events_and_count_the_rest() {
        let (feed, mut live) = tokio::sync::mpsc::unbounded_channel();
        let mut options = SubscribeOptions::new(Limits::interactive(), CancellationToken::new());
        options.feed = Some(feed);
        let mut collector = Collector::new(&options, Instant::now());
        let total = INTERACTIVE_KEPT_EVENTS + 5;
        for n in 0..total {
            assert!(!collector.push(json!({"data": n})));
        }
        let collected = collector.finish();
        assert_eq!(collected.events.len(), INTERACTIVE_KEPT_EVENTS);
        assert_eq!(collected.event_count, total);
        assert_eq!(collected.events[0].payload, json!({"data": 0}));
        let mut fed = 0;
        while live.try_recv().is_ok() {
            fed += 1;
        }
        assert_eq!(fed, total, "live views see every event");
    }

    #[test]
    fn waits_end_with_the_duration() {
        let headless = SubscribeOptions::new(
            Limits {
                max_events: None,
                duration: Some(Duration::from_secs(1)),
            },
            CancellationToken::new(),
        );
        let now = Instant::now();
        let ack = headless.within(now, Duration::from_secs(10));
        assert!(ack <= Duration::from_secs(1), "{ack:?}");
        let short = headless.within(now, Duration::from_millis(5));
        assert_eq!(short, Duration::from_millis(5));
        if let Some(gone) = now.checked_sub(Duration::from_secs(2)) {
            assert_eq!(headless.within(gone, DEFAULT_ACK_TIMEOUT), Duration::ZERO);
        }
        let interactive = SubscribeOptions::new(Limits::interactive(), CancellationToken::new());
        assert_eq!(
            interactive.within(now, DEFAULT_ACK_TIMEOUT),
            DEFAULT_ACK_TIMEOUT
        );
    }

    #[test]
    fn headless_limits_default_to_ten_events_and_thirty_seconds() {
        let text = "version: 1\nkind: graphql\nname: s\nendpoint: ws://h\ndocument: 'subscription { a }'\n";
        let graphql = GraphqlRequest::parse(std::path::Path::new("s.rman.yaml"), text).unwrap();
        let limits = Limits::headless(&graphql, None, None);
        assert_eq!(limits.max_events, Some(10));
        assert_eq!(limits.duration, Some(Duration::from_secs(30)));
        let overridden = Limits::headless(&graphql, Some(3), Some(Duration::from_secs(1)));
        assert_eq!(overridden.max_events, Some(3));
        assert_eq!(overridden.duration, Some(Duration::from_secs(1)));
        assert_eq!(Limits::interactive().max_events, None);
    }

    #[tokio::test]
    async fn the_collector_stops_at_max_events_and_feeds_live_views() {
        let (feed, mut live) = tokio::sync::mpsc::unbounded_channel();
        let mut options = SubscribeOptions::new(
            Limits {
                max_events: Some(2),
                duration: None,
            },
            CancellationToken::new(),
        );
        options.feed = Some(feed);
        let mut collector = Collector::new(&options, Instant::now());
        assert!(!collector.push(json!({"data": 1})));
        assert!(collector.push(json!({"data": 2})));
        assert_eq!(live.recv().await.unwrap().payload, json!({"data": 1}));
        assert_eq!(collector.finish().events.len(), 2);
        assert!(Ended::MaxEvents.by_client() && !Ended::Complete.by_client());
    }
}
