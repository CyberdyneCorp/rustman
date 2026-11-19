//! Subscriptions as a generic triple (design D4): call `subscribe` with the
//! request's params, take its result as the subscription id, collect the
//! `notification` messages that carry that id (at `notification_id_path`,
//! `$.params.subscription` by default), then call `unsubscribe` with the
//! id. Headless runs stop at `max_events` or `max_duration_ms`; the TUI
//! runs until the user stops it.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use rustman_core::Redactor;
use rustman_core::model::SubscriptionSpec;
use rustman_core::run::redact_value;
use rustman_core::vars::ResolvedCall;
use serde_json::Value;
use serde_json_path::JsonPath;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::envelope::{Envelope, IdSequence, is_response, response_id};
use crate::error::JsonrpcError;
use crate::outcome::{CallOutcome, CallResult, single_result};
use crate::ws::{TEARDOWN_TIMEOUT, WsConnection};

/// The most events kept in memory; older ones are dropped (the count
/// goes on).
pub const MAX_KEPT_EVENTS: usize = 10_000;

/// When a subscription stops by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubscriptionLimits {
    pub max_events: Option<u32>,
    pub max_duration: Option<Duration>,
}

impl SubscriptionLimits {
    /// The headless bounds of `spec`: `max_events` (default 10) and
    /// `max_duration_ms` (default 30000).
    pub fn headless(spec: &SubscriptionSpec) -> Self {
        Self {
            max_events: Some(spec.effective_max_events()),
            max_duration: Some(Duration::from_millis(spec.effective_max_duration_ms())),
        }
    }

    /// No bounds: the subscription runs until it is stopped.
    pub fn interactive() -> Self {
        Self::default()
    }
}

/// How a subscription runs: its bounds (`None` takes the headless bounds
/// of the request), a stop signal, and an optional live feed of events,
/// each redacted with `redactor` before it is sent.
#[derive(Clone, Default)]
pub struct SubscribeOptions {
    pub limits: Option<SubscriptionLimits>,
    pub cancel: CancellationToken,
    pub events: Option<UnboundedSender<Value>>,
    pub redactor: Redactor,
}

impl fmt::Debug for SubscribeOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscribeOptions")
            .field("limits", &self.limits)
            .field("cancel", &self.cancel)
            .field("events", &self.events.is_some())
            .finish_non_exhaustive()
    }
}

impl SubscribeOptions {
    fn limits_for(&self, spec: &SubscriptionSpec) -> SubscriptionLimits {
        self.limits
            .unwrap_or_else(|| SubscriptionLimits::headless(spec))
    }
}

/// Why a subscription ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionEnd {
    MaxEvents,
    Duration,
    /// Stopped by the caller (`Esc` in the TUI, Ctrl-C in a run).
    Stopped,
    /// The server closed the connection.
    Closed,
}

impl SubscriptionEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxEvents => "max_events",
            Self::Duration => "duration",
            Self::Stopped => "stopped",
            Self::Closed => "closed",
        }
    }
}

/// What a subscription collected.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionRecord {
    /// The result of the subscribe call.
    pub subscription_id: Value,
    /// The notification messages, newest last (at most [`MAX_KEPT_EVENTS`]).
    pub events: Vec<Value>,
    /// Every event received, kept or not.
    pub event_count: u64,
    pub ended: SubscriptionEnd,
    /// Whether the unsubscribe call was sent.
    pub unsubscribed: bool,
    pub elapsed: Duration,
}

/// The outcome of [`run`]: the subscribe call and what it collected, plus
/// the subscribe envelope and its response as sent and received.
pub(crate) struct Run {
    pub result: Result<(CallResult, Option<SubscriptionRecord>), JsonrpcError>,
    pub sent: String,
    pub received: Option<String>,
}

/// Subscribe on `connection`, collect, then unsubscribe. A subscribe call
/// answered with an error object ends there, with no record.
pub(crate) async fn run(
    connection: &mut WsConnection,
    spec: &SubscriptionSpec,
    call: &ResolvedCall,
    options: &SubscribeOptions,
    timeout: Duration,
) -> Run {
    let started = Instant::now();
    let id_path = match JsonPath::parse(spec.effective_notification_id_path()) {
        Ok(path) => path,
        Err(e) => return failed(format!("invalid notification_id_path: {e}"), String::new()),
    };
    let mut subscribe = ResolvedCall {
        method: spec.subscribe.clone(),
        notification: false,
        ..call.clone()
    };
    if subscribe.id.is_none() {
        subscribe.id = Some(connection.next_id());
    }
    let envelope = Envelope::of(&subscribe, &mut IdSequence::default());
    let sent = envelope.value.to_string();
    let mut collector = Collector::new(spec, &id_path, options, started);
    let (response, received) =
        match subscribe_call(connection, &envelope, &mut collector, timeout).await {
            Ok(answer) => answer,
            Err(e) => return failed_with(e, sent),
        };
    let call = match single_result(&envelope, &response) {
        Ok(call) => call,
        Err(e) => return failed_with(e, sent),
    };
    let CallOutcome::Result(subscription_id) = &call.outcome else {
        return Run {
            result: Ok((call, None)),
            sent,
            received: Some(received),
        };
    };
    collector.subscription_id = Some(subscription_id.clone());
    let ended = collector.collect(connection).await;
    let unsubscribed =
        ended != SubscriptionEnd::Closed && unsubscribe(connection, spec, subscription_id).await;
    let record = collector.finish(ended, unsubscribed);
    Run {
        result: Ok((call, Some(record))),
        sent,
        received: Some(received),
    }
}

fn failed(message: String, sent: String) -> Run {
    failed_with(JsonrpcError::Unsupported(message), sent)
}

fn failed_with(error: JsonrpcError, sent: String) -> Run {
    Run {
        result: Err(error),
        sent,
        received: None,
    }
}

/// Send the subscribe call and wait for its response. Notifications that
/// arrive first are kept: they may belong to the new subscription.
async fn subscribe_call(
    connection: &mut WsConnection,
    envelope: &Envelope,
    collector: &mut Collector<'_>,
    timeout: Duration,
) -> Result<(Value, String), JsonrpcError> {
    connection.send_text(envelope.value.to_string()).await?;
    let id = envelope.id.clone().unwrap_or(Value::Null);
    let wanted = |v: &Value| is_response(v) && answers(v, &id);
    let early = &mut collector.early;
    let mut keep = |value: Value| {
        if early.len() < MAX_KEPT_EVENTS {
            early.push(value);
        }
    };
    connection.read_matching(wanted, &mut keep, timeout).await
}

/// Whether the response `value` answers the call `id`: it carries that id,
/// or it is an error object with a null id (the server could not read the
/// id). A result with a null id answers nothing.
fn answers(value: &Value, id: &Value) -> bool {
    let answered = response_id(value);
    answered == id || (answered.is_null() && value.get("error").is_some())
}

/// Call `unsubscribe` with the subscription id and wait briefly for its
/// answer. Whether the call was sent.
async fn unsubscribe(
    connection: &mut WsConnection,
    spec: &SubscriptionSpec,
    subscription_id: &Value,
) -> bool {
    let id = connection.next_id();
    let envelope = serde_json::json!({
        "jsonrpc": "2.0",
        "method": spec.unsubscribe,
        "params": [subscription_id],
        "id": id,
    });
    if connection.send_text(envelope.to_string()).await.is_err() {
        return false;
    }
    let wanted = |v: &Value| is_response(v) && response_id(v) == &id;
    let _ = connection
        .read_matching(wanted, &mut |_| {}, TEARDOWN_TIMEOUT)
        .await;
    true
}

/// Collects the notifications of one subscription within its limits.
struct Collector<'a> {
    spec: &'a SubscriptionSpec,
    id_path: &'a JsonPath,
    options: &'a SubscribeOptions,
    limits: SubscriptionLimits,
    started: Instant,
    subscription_id: Option<Value>,
    /// Frames read before the subscribe response.
    early: Vec<Value>,
    events: VecDeque<Value>,
    count: u64,
}

impl<'a> Collector<'a> {
    fn new(
        spec: &'a SubscriptionSpec,
        id_path: &'a JsonPath,
        options: &'a SubscribeOptions,
        started: Instant,
    ) -> Self {
        Self {
            spec,
            id_path,
            options,
            limits: options.limits_for(spec),
            started,
            subscription_id: None,
            early: Vec::new(),
            events: VecDeque::new(),
            count: 0,
        }
    }

    fn belongs(&self, message: &Value) -> bool {
        if message.get("method").and_then(Value::as_str) != Some(self.spec.notification.as_str()) {
            return false;
        }
        let found = self.id_path.query(message).first().cloned();
        found.is_some() && found.as_ref() == self.subscription_id.as_ref()
    }

    /// Record `message` if it is one of ours; whether the event bound is
    /// reached.
    fn offer(&mut self, message: Value) -> bool {
        if !self.belongs(&message) {
            return false;
        }
        if let Some(feed) = &self.options.events {
            let _ = feed.send(redact_value(&message, &self.options.redactor));
        }
        if self.events.len() == MAX_KEPT_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(message);
        self.count += 1;
        self.limits
            .max_events
            .is_some_and(|max| self.count >= u64::from(max))
    }

    async fn deadline(&self) {
        match self.limits.max_duration {
            Some(limit) => tokio::time::sleep_until((self.started + limit).into()).await,
            None => std::future::pending().await,
        }
    }

    async fn collect(&mut self, connection: &mut WsConnection) -> SubscriptionEnd {
        for message in std::mem::take(&mut self.early) {
            if self.offer(message) {
                return SubscriptionEnd::MaxEvents;
            }
        }
        loop {
            let next = tokio::select! {
                biased;
                () = self.options.cancel.cancelled() => return SubscriptionEnd::Stopped,
                () = self.deadline() => return SubscriptionEnd::Duration,
                next = connection.next_json() => next,
            };
            let Ok((message, _)) = next else {
                return SubscriptionEnd::Closed;
            };
            if self.offer(message) {
                return SubscriptionEnd::MaxEvents;
            }
        }
    }

    fn finish(self, ended: SubscriptionEnd, unsubscribed: bool) -> SubscriptionRecord {
        SubscriptionRecord {
            subscription_id: self.subscription_id.unwrap_or(Value::Null),
            events: self.events.into(),
            event_count: self.count,
            ended,
            unsubscribed,
            elapsed: self.started.elapsed(),
        }
    }
}
