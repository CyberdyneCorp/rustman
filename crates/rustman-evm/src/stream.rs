//! Head and log streams (evm-toolkit "Head and log streams", design D6).
//!
//! Over WebSocket a stream is an `eth_subscribe` subscription through the
//! JSON-RPC client. Over HTTP it polls `eth_getFilterChanges` on a filter
//! from `eth_newFilter` (or `eth_newBlockFilter`) every 2 s by default,
//! creating a new filter when the node forgets it. Logs with
//! `removed: true` are reorgs: they are passed on (shown as `REORGED`) but
//! not counted as new events.

use std::time::{Duration, Instant};

use alloy_json_abi::Event;
use alloy_primitives::{Address, B256};
use rustman_core::model::{EndpointScheme, SubscriptionSpec};
use rustman_core::vars::{ResolvedCall, ResolvedJsonrpc};
use rustman_jsonrpc::{
    CallOutcome, JsonrpcClient, SubscribeOptions, SubscriptionEnd, SubscriptionLimits, check_target,
};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

use crate::call::parse_address;
use crate::error::EvmError;
use crate::rpc::{JsonrpcTransport, Rpc, RpcFailure, call};

/// The polling interval over HTTP.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What to stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Heads,
    Logs(LogFilter),
}

/// Which logs: from any of `addresses` (all when empty), matching
/// `topics` position by position (`None` matches anything).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub addresses: Vec<Address>,
    pub topics: Vec<Option<Vec<B256>>>,
}

impl LogFilter {
    /// A filter from user input: addresses, an optional event signature
    /// (`Transfer(address,address,uint256)`) for topic 0, then further
    /// topics (32-byte hex, a signature, or `*` / empty for any).
    pub fn parse(
        addresses: &[String],
        event: Option<&str>,
        topics: &[String],
    ) -> Result<Self, EvmError> {
        let addresses = addresses
            .iter()
            .map(|a| parse_address(a).map_err(|e| EvmError::config(format!("address: {e}"))))
            .collect::<Result<_, _>>()?;
        let mut all = Vec::new();
        if let Some(event) = event {
            all.push(Some(vec![topic(event)?]));
        }
        for text in topics {
            let any = text.trim().is_empty() || text.trim() == "*";
            all.push(if any { None } else { Some(vec![topic(text)?]) });
        }
        Ok(Self {
            addresses,
            topics: all,
        })
    }

    /// The filter object of `eth_subscribe("logs", ...)` and
    /// `eth_newFilter`.
    pub fn to_json(&self) -> Value {
        let mut filter = Map::new();
        let addresses: Vec<String> = self.addresses.iter().map(|a| a.to_checksum(None)).collect();
        match addresses.as_slice() {
            [] => {}
            [one] => {
                filter.insert("address".into(), json!(one));
            }
            _ => {
                filter.insert("address".into(), json!(addresses));
            }
        }
        if !self.topics.is_empty() {
            let topics: Vec<Value> = self.topics.iter().map(topic_json).collect();
            filter.insert("topics".into(), Value::Array(topics));
        }
        Value::Object(filter)
    }
}

fn topic_json(topic: &Option<Vec<B256>>) -> Value {
    match topic.as_deref() {
        None => Value::Null,
        Some([one]) => json!(one.to_string()),
        Some(many) => json!(many.iter().map(ToString::to_string).collect::<Vec<_>>()),
    }
}

/// A topic: 32-byte hex, or an event signature hashed to its selector.
pub fn topic(text: &str) -> Result<B256, EvmError> {
    let text = text.trim();
    if text.contains('(') {
        let event =
            Event::parse(text).map_err(|e| EvmError::config(format!("event '{text}': {e}")))?;
        return Ok(event.selector());
    }
    text.parse().map_err(|_| {
        EvmError::config(format!(
            "topic '{text}' is not 32-byte hex or an event signature"
        ))
    })
}

/// When a stream stops by itself, and the stop signal.
#[derive(Clone, Debug)]
pub struct StreamConfig {
    pub poll_interval: Duration,
    /// New events (reorgs not counted) after which the stream ends.
    pub max_events: Option<u64>,
    pub max_duration: Option<Duration>,
    pub cancel: CancellationToken,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            max_events: None,
            max_duration: None,
            cancel: CancellationToken::new(),
        }
    }
}

/// One streamed item: a block header, or a log as the node sent it.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamItem {
    Head(Value),
    Log(Value),
}

impl StreamItem {
    /// A log removed by a reorg.
    pub fn removed(&self) -> bool {
        match self {
            Self::Log(log) => log["removed"].as_bool().unwrap_or(false),
            Self::Head(_) => false,
        }
    }
}

/// How the stream is fed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    Subscription,
    Polling(Duration),
}

impl StreamMode {
    /// `eth_subscribe` for `ws://` and `wss://`, polling otherwise.
    pub fn of(url: &str, config: &StreamConfig) -> Self {
        match EndpointScheme::of(url) {
            Some(EndpointScheme::WebSocket) => Self::Subscription,
            _ => Self::Polling(config.poll_interval),
        }
    }

    /// The note shown when polling: `polling every 2s (no WebSocket)`.
    pub fn note(&self) -> Option<String> {
        let Self::Polling(interval) = self else {
            return None;
        };
        let every = if interval.subsec_millis() == 0 {
            format!("{}s", interval.as_secs())
        } else {
            format!("{}ms", interval.as_millis())
        };
        Some(format!("polling every {every} (no WebSocket)"))
    }
}

/// How a stream went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSummary {
    pub mode: StreamMode,
    /// New events, reorged logs excluded.
    pub events: u64,
    pub reorged: u64,
    pub ended: SubscriptionEnd,
}

/// How many streamed items may wait for the consumer: when it falls
/// behind, the stream waits (and stops reading the node) instead of
/// buffering without bound.
pub const STREAM_BUFFER: usize = 256;

/// The channel [`stream`] sends its items into, bounded by
/// [`STREAM_BUFFER`].
pub fn item_channel() -> (Sender<StreamItem>, Receiver<StreamItem>) {
    tokio::sync::mpsc::channel(STREAM_BUFFER)
}

/// Counts items as they are passed on.
struct Counter<'a> {
    items: &'a Sender<StreamItem>,
    max_events: Option<u64>,
    events: u64,
    reorged: u64,
    /// The consumer is gone: the stream ends.
    closed: bool,
}

impl<'a> Counter<'a> {
    fn new(items: &'a Sender<StreamItem>, config: &StreamConfig) -> Self {
        Self {
            items,
            max_events: config.max_events,
            events: 0,
            reorged: 0,
            closed: false,
        }
    }

    /// Pass `item` on, waiting while the consumer is behind; whether the
    /// stream should end (`max_events` reached or the consumer gone).
    async fn offer(&mut self, item: StreamItem) -> bool {
        if item.removed() {
            self.reorged += 1;
        } else {
            self.events += 1;
        }
        if self.items.send(item).await.is_err() {
            self.closed = true;
        }
        self.full()
    }

    /// Why a stream that is [`Self::full`] ended.
    fn end(&self) -> SubscriptionEnd {
        match self.closed {
            true => SubscriptionEnd::Stopped,
            false => SubscriptionEnd::MaxEvents,
        }
    }

    fn full(&self) -> bool {
        self.closed || self.max_events.is_some_and(|max| self.events >= max)
    }

    fn summary(&self, mode: StreamMode, ended: SubscriptionEnd) -> StreamSummary {
        StreamSummary {
            mode,
            events: self.events,
            reorged: self.reorged,
            ended,
        }
    }
}

/// Stream `kind` from `transport`'s endpoint into `items` until a bound
/// is reached, `config.cancel` fires or the node closes the stream.
pub async fn stream(
    transport: &JsonrpcTransport,
    kind: &StreamKind,
    config: &StreamConfig,
    items: &Sender<StreamItem>,
) -> Result<StreamSummary, EvmError> {
    match StreamMode::of(&transport.request().url.text, config) {
        StreamMode::Subscription => subscribe(transport, kind, config, items).await,
        StreamMode::Polling(_) => poll(transport, kind, config, items).await,
    }
}

fn subscribe_params(kind: &StreamKind) -> Value {
    match kind {
        StreamKind::Heads => json!(["newHeads"]),
        StreamKind::Logs(filter) => json!(["logs", filter.to_json()]),
    }
}

async fn subscribe(
    transport: &JsonrpcTransport,
    kind: &StreamKind,
    config: &StreamConfig,
    items: &Sender<StreamItem>,
) -> Result<StreamSummary, EvmError> {
    let call = ResolvedCall::new("eth_subscribe", Some(subscribe_params(kind)));
    let mut target = ResolvedJsonrpc::new(transport.request().clone(), vec![call], false);
    target.subscription = Some(SubscriptionSpec {
        subscribe: "eth_subscribe".to_owned(),
        unsubscribe: "eth_unsubscribe".to_owned(),
        notification: "eth_subscription".to_owned(),
        notification_id_path: None,
        max_events: None,
        max_duration_ms: None,
    });
    if let Some(refusal) = check_target(&target) {
        return Err(EvmError::Refused(refusal.message));
    }
    let cancel = config.cancel.child_token();
    let (feed, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    let options = SubscribeOptions {
        limits: Some(SubscriptionLimits {
            max_events: None,
            max_duration: config.max_duration,
        }),
        cancel: cancel.clone(),
        events: Some(feed),
        redactor: transport.redactor(),
    };
    let client = JsonrpcClient::new(transport.sender()).with_subscribe(options);
    let mut counter = Counter::new(items, config);
    let send = client.send(&target, transport.options());
    tokio::pin!(send);
    let exchange = loop {
        tokio::select! {
            exchange = &mut send => break exchange,
            Some(message) = notifications.recv() => {
                let item = notified(kind, message).filter(|_| !counter.full());
                if let Some(item) = item
                    && counter.offer(item).await
                {
                    cancel.cancel();
                }
            }
        }
    };
    while let Ok(message) = notifications.try_recv() {
        if let Some(item) = notified(kind, message).filter(|_| !counter.full()) {
            counter.offer(item).await;
        }
    }
    if let Some(error) = exchange.error {
        return Err(EvmError::Rpc(format!("eth_subscribe: {error}")));
    }
    if let Some(CallOutcome::Error(error)) = exchange.calls.first().map(|c| &c.outcome) {
        return Err(EvmError::Rpc(format!(
            "eth_subscribe: rpc error {}: {}",
            error.code, error.message
        )));
    }
    let ended = match exchange.subscription.map(|s| s.ended) {
        Some(SubscriptionEnd::Stopped) if counter.full() => counter.end(),
        Some(ended) => ended,
        None => SubscriptionEnd::Closed,
    };
    Ok(counter.summary(StreamMode::Subscription, ended))
}

/// The item of an `eth_subscription` notification.
fn notified(kind: &StreamKind, message: Value) -> Option<StreamItem> {
    let result = message.get("params")?.get("result")?.clone();
    Some(match kind {
        StreamKind::Heads => StreamItem::Head(result),
        StreamKind::Logs(_) => StreamItem::Log(result),
    })
}

/// Poll a filter on `rpc` every `config.poll_interval`.
pub async fn poll(
    rpc: &dyn Rpc,
    kind: &StreamKind,
    config: &StreamConfig,
    items: &Sender<StreamItem>,
) -> Result<StreamSummary, EvmError> {
    let mode = StreamMode::Polling(config.poll_interval);
    let started = Instant::now();
    let mut counter = Counter::new(items, config);
    let mut filter = new_filter(rpc, kind).await?;
    let ended = loop {
        if let Some(ended) = wait(config, started).await {
            break ended;
        }
        let changes = match rpc.call("eth_getFilterChanges", json!([filter])).await {
            Ok(changes) => changes,
            Err(failure) if filter_lost(&failure) => {
                filter = new_filter(rpc, kind).await?;
                continue;
            }
            Err(failure) => return Err(failure.into_error("eth_getFilterChanges")),
        };
        if offer_changes(rpc, kind, changes, &mut counter).await {
            break counter.end();
        }
    };
    let _ = rpc.call("eth_uninstallFilter", json!([filter])).await;
    Ok(counter.summary(mode, ended))
}

/// Wait one interval; why the stream ends, if it does meanwhile.
async fn wait(config: &StreamConfig, started: Instant) -> Option<SubscriptionEnd> {
    let deadline = config.max_duration.map(|max| started + max);
    let sleep = tokio::time::sleep(config.poll_interval);
    let until = async {
        match deadline {
            Some(at) => tokio::time::sleep_until(at.into()).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        biased;
        () = config.cancel.cancelled() => Some(SubscriptionEnd::Stopped),
        () = until => Some(SubscriptionEnd::Duration),
        () = sleep => None,
    }
}

async fn new_filter(rpc: &dyn Rpc, kind: &StreamKind) -> Result<Value, EvmError> {
    match kind {
        StreamKind::Heads => call(rpc, "eth_newBlockFilter", json!([])).await,
        StreamKind::Logs(filter) => call(rpc, "eth_newFilter", json!([filter.to_json()])).await,
    }
}

fn filter_lost(failure: &RpcFailure) -> bool {
    matches!(failure, RpcFailure::Error { message, .. }
        if message.to_ascii_lowercase().contains("filter not found"))
}

/// Pass on the items of one `eth_getFilterChanges` answer; whether
/// `max_events` is reached.
async fn offer_changes(
    rpc: &dyn Rpc,
    kind: &StreamKind,
    changes: Value,
    counter: &mut Counter<'_>,
) -> bool {
    let Value::Array(changes) = changes else {
        return false;
    };
    for change in changes {
        let item = match kind {
            StreamKind::Logs(_) => StreamItem::Log(change),
            StreamKind::Heads => StreamItem::Head(head(rpc, change).await),
        };
        if counter.offer(item).await {
            return true;
        }
    }
    false
}

/// The header of the block whose hash a block filter returned, or just
/// the hash when the node does not answer.
async fn head(rpc: &dyn Rpc, hash: Value) -> Value {
    match rpc.call("eth_getBlockByHash", json!([hash, false])).await {
        Ok(block @ Value::Object(_)) => block,
        _ => json!({"hash": hash}),
    }
}

/// `#<number> <hash>` for a head.
pub fn head_line(head: &Value) -> String {
    let number = head["number"]
        .as_str()
        .and_then(|n| crate::quantity::parse_hex_u64(n).ok())
        .map_or_else(|| "?".to_owned(), |n| n.to_string());
    let hash = head["hash"].as_str().unwrap_or("?");
    format!("block #{number} {hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_take_addresses_events_and_topics() {
        let filter = LogFilter::parse(
            &["0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_owned()],
            Some("Transfer(address,address,uint256)"),
            &["*".to_owned()],
        )
        .unwrap();
        assert_eq!(
            filter.to_json(),
            json!({
                "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef", null],
            })
        );
        assert!(LogFilter::parse(&["0x1".to_owned()], None, &[]).is_err());
        assert!(LogFilter::parse(&[], None, &["nope".to_owned()]).is_err());
        assert_eq!(LogFilter::default().to_json(), json!({}));
    }

    #[test]
    fn the_polling_note() {
        let config = StreamConfig::default();
        let mode = StreamMode::of("https://node.example", &config);
        assert_eq!(
            mode.note().as_deref(),
            Some("polling every 2s (no WebSocket)")
        );
        assert_eq!(StreamMode::of("wss://node.example", &config).note(), None);
        let fast = StreamMode::Polling(Duration::from_millis(500));
        assert_eq!(
            fast.note().as_deref(),
            Some("polling every 500ms (no WebSocket)")
        );
    }

    #[test]
    fn heads_are_shown_by_number() {
        let head = json!({"number": "0x10", "hash": "0xab"});
        assert_eq!(head_line(&head), "block #16 0xab");
    }
}
