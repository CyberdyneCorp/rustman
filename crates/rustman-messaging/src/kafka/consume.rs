//! Kafka consume: every chosen partition is read from a resolved start
//! offset by its own task, with no consumer group and no commits. The
//! collector keeps at most `max_messages` messages, each value cut to
//! `max_message_bytes`, and stops at its bounds or when cancelled. The
//! records waiting for it take at most [`QUEUE_BYTES`].

use std::time::Duration;

use rskafka::chrono::{DateTime, Utc};
use rskafka::client::Client;
use rskafka::client::partition::{OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::record::RecordAndOffset;
use rustman_core::model::StartOffset;
use rustman_core::vars::ResolvedMessaging;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{check_partitions, request_error, topic_partitions};
use crate::payload::{Header, cut, render};
use crate::registry::RegistryClient;
use crate::{
    Bounds, ConsumedMessage, MessagingError, MessagingExchange, RunOptions, sleep_until,
    until_stopped,
};

/// Bytes asked for per fetch.
pub(crate) const FETCH_MAX_BYTES: i32 = 4 * 1024 * 1024;

/// How long the broker may hold a fetch that has nothing to return.
const FETCH_WAIT_MS: i32 = 250;

/// The most records waiting for the collector.
const QUEUE: usize = 256;

/// The bytes the records waiting for the collector may take, at most.
pub const QUEUE_BYTES: usize = 64 * 1024 * 1024;

/// How many fetched records may wait for the collector when each can be
/// up to `frame_bytes` (see [`super::max_frame_bytes`]): at most
/// [`QUEUE_BYTES`] in all, and between 1 and [`QUEUE`] records.
pub fn queue_capacity(frame_bytes: usize) -> usize {
    (QUEUE_BYTES / frame_bytes.max(1)).clamp(1, QUEUE)
}

type Fetched = Result<(i32, RecordAndOffset), MessagingError>;

/// One consume.
pub struct Consume<'a> {
    pub client: &'a Client,
    pub resolved: &'a ResolvedMessaging,
    pub registry: Option<&'a RegistryClient>,
}

impl Consume<'_> {
    pub async fn run(
        &self,
        mut options: RunOptions,
        exchange: &mut MessagingExchange,
    ) -> Result<(), MessagingError> {
        let topic = self.resolved.topic.clone().unwrap_or_default();
        let existing = topic_partitions(self.client, &topic).await?;
        let wanted = self.resolved.partitions.clone().unwrap_or(existing.clone());
        check_partitions(&topic, &existing, &wanted)?;
        let threshold = match &self.resolved.offset {
            StartOffset::Timestamp(at) => Some(parse_timestamp(at)?),
            _ => None,
        };
        let mut readers = JoinSet::new();
        let capacity = queue_capacity(super::max_frame_bytes(&self.resolved.limits));
        let (tx, mut rx) = mpsc::channel(capacity);
        for partition in wanted {
            let client = self.partition(&topic, partition).await?;
            let start = start_offset(&client, &self.resolved.offset, threshold).await?;
            readers.spawn(read_partition(client, start, tx.clone(), topic.clone()));
        }
        drop(tx);
        if let Some(ready) = options.ready.take() {
            let _ = ready.send(());
        }
        let collected = self.collect(&options, threshold, &mut rx, exchange).await;
        readers.abort_all();
        collected
    }

    async fn partition(
        &self,
        topic: &str,
        partition: i32,
    ) -> Result<PartitionClient, MessagingError> {
        self.client
            .partition_client(topic.to_owned(), partition, UnknownTopicHandling::Error)
            .await
            .map_err(|e| request_error(e, topic))
    }

    /// Record fetched messages until the bounds are reached, the readers
    /// end, or the run is cancelled.
    async fn collect(
        &self,
        options: &RunOptions,
        threshold: Option<DateTime<Utc>>,
        rx: &mut mpsc::Receiver<Fetched>,
        exchange: &mut MessagingExchange,
    ) -> Result<(), MessagingError> {
        let deadline = options.bounds.map(|b| Instant::now() + b.duration);
        let max = options.bounds.map_or(u64::MAX, |b: Bounds| b.max_messages);
        let max_bytes = usize::try_from(self.resolved.limits.effective_max_message_bytes())
            .unwrap_or(usize::MAX);
        while (exchange.messages.len() as u64) < max {
            let fetched = tokio::select! {
                () = options.cancel.cancelled() => return Ok(()),
                () = sleep_until(deadline) => return Ok(()),
                fetched = rx.recv() => fetched,
            };
            let Some(fetched) = fetched else {
                return Ok(());
            };
            let (partition, record) = fetched?;
            if threshold.is_some_and(|t| record.record.timestamp < t) {
                continue;
            }
            let decoding = record_message(self.registry, partition, record, max_bytes, exchange);
            let Some(message) = until_stopped(&options.cancel, deadline, decoding).await else {
                return Ok(());
            };
            if let Some(feed) = &options.feed {
                let _ = feed.send(message.clone());
            }
            exchange.messages.push(message);
        }
        Ok(())
    }
}

/// The recorded form of `record`, decoded through `registry` when one is
/// configured (a registry failure is a warning). The key and the value
/// are each kept up to `max_bytes`.
async fn record_message(
    registry: Option<&RegistryClient>,
    partition: i32,
    record: RecordAndOffset,
    max_bytes: usize,
    exchange: &mut MessagingExchange,
) -> ConsumedMessage {
    let RecordAndOffset { record, offset } = record;
    let (key, key_schema_id) = match &record.key {
        Some(key) => decode(registry, key, max_bytes, exchange).await,
        None => (None, None),
    };
    let size = record.value.as_ref().map_or(0, Vec::len);
    let (value, schema_id) = match &record.value {
        Some(value) => decode(registry, value, max_bytes, exchange).await,
        None => (None, None),
    };
    ConsumedMessage {
        partition: Some(partition),
        offset: Some(offset),
        timestamp: Some(timestamp_text(record.timestamp)),
        key,
        headers: record
            .headers
            .iter()
            .map(|(name, value)| Header::from_bytes(name, value))
            .collect(),
        value,
        size,
        truncated: size > max_bytes,
        schema_id,
        key_schema_id,
        ..ConsumedMessage::default()
    }
}

async fn decode(
    registry: Option<&RegistryClient>,
    bytes: &[u8],
    max_bytes: usize,
    exchange: &mut MessagingExchange,
) -> (Option<serde_json::Value>, Option<u32>) {
    if let Some(registry) = registry {
        match registry.decode(bytes).await {
            Some(Ok(decoded)) => return (Some(decoded.value), Some(decoded.schema_id)),
            Some(Err(warning)) if !exchange.warnings.contains(&warning) => {
                exchange.warnings.push(warning);
            }
            _ => {}
        }
    }
    (Some(render(cut(bytes, max_bytes))), None)
}

/// The first offset to read in `client`'s partition. Absolute offsets are
/// kept within the partition's current range; a timestamp after the last
/// message starts at the end.
async fn start_offset(
    client: &PartitionClient,
    offset: &StartOffset,
    threshold: Option<DateTime<Utc>>,
) -> Result<i64, MessagingError> {
    let topic = client.topic().to_owned();
    let get = |at| async move { client.get_offset(at).await };
    let fail = |e| request_error(e, &topic);
    match (offset, threshold) {
        (StartOffset::Earliest, _) => get(OffsetAt::Earliest).await.map_err(fail),
        (StartOffset::Absolute(n), _) => {
            let earliest = get(OffsetAt::Earliest).await.map_err(fail)?;
            let latest = get(OffsetAt::Latest).await.map_err(fail)?;
            Ok((*n).clamp(earliest, latest.max(earliest)))
        }
        (StartOffset::Timestamp(_), Some(at)) => {
            let found = get(OffsetAt::Timestamp(at)).await.map_err(fail)?;
            match found {
                n if n >= 0 => Ok(n),
                _ => get(OffsetAt::Latest).await.map_err(fail),
            }
        }
        _ => get(OffsetAt::Latest).await.map_err(fail),
    }
}

/// Fetch `client`'s partition from `offset` on, forever, sending each
/// record to `tx`; stops when the collector is gone or a fetch fails.
async fn read_partition(
    client: PartitionClient,
    mut offset: i64,
    tx: mpsc::Sender<Fetched>,
    topic: String,
) {
    let partition = client.partition();
    loop {
        let fetched = client
            .fetch_records(offset, 1..FETCH_MAX_BYTES, FETCH_WAIT_MS)
            .await;
        let records = match fetched {
            Ok((records, _high_watermark)) => records,
            Err(e) => {
                let _ = tx.send(Err(request_error(e, &topic))).await;
                return;
            }
        };
        if records.is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for record in records {
            offset = offset.max(record.offset + 1);
            if tx.send(Ok((partition, record))).await.is_err() {
                return;
            }
        }
    }
}

/// An RFC 3339 time as rskafka takes it.
pub fn parse_timestamp(text: &str) -> Result<DateTime<Utc>, MessagingError> {
    let invalid = || {
        MessagingError::Invalid(format!(
            "kafka: offset timestamp '{text}' is not an RFC 3339 time"
        ))
    };
    let at: jiff::Timestamp = text.trim().parse().map_err(|_| invalid())?;
    DateTime::from_timestamp_millis(at.as_millisecond()).ok_or_else(invalid)
}

/// A record timestamp as RFC 3339 with milliseconds.
pub fn timestamp_text(at: DateTime<Utc>) -> String {
    jiff::Timestamp::from_millisecond(at.timestamp_millis())
        .map(|t| format!("{t:.3}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_from_rfc3339_and_print_back() {
        let at = parse_timestamp("2026-11-17T09:00:00Z").unwrap();
        assert_eq!(at.timestamp_millis(), 1_794_906_000_000);
        assert_eq!(timestamp_text(at), "2026-11-17T09:00:00.000Z");
        let offset = parse_timestamp("2026-11-17T10:00:00+01:00").unwrap();
        assert_eq!(offset, at);
        let err = parse_timestamp("yesterday").unwrap_err();
        assert_eq!(
            err.to_string(),
            "kafka: offset timestamp 'yesterday' is not an RFC 3339 time"
        );
    }

    #[tokio::test]
    async fn keys_and_values_are_both_kept_up_to_the_byte_limit() {
        use rskafka::record::Record;
        let record = RecordAndOffset {
            record: Record {
                key: Some(vec![b'k'; 100]),
                value: Some(vec![b'v'; 100]),
                headers: std::collections::BTreeMap::new(),
                timestamp: parse_timestamp("2026-11-17T09:00:00Z").unwrap(),
            },
            offset: 3,
        };
        let mut exchange = MessagingExchange {
            protocol: "kafka",
            operation: "consume",
            brokers: String::new(),
            destination: "t".to_owned(),
            queue: None,
            security: None,
            produced: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
            asyncapi: None,
            duration_ms: 0,
        };
        let message = record_message(None, 0, record, 10, &mut exchange).await;
        assert_eq!(message.key, Some(serde_json::json!("kkkkkkkkkk")));
        assert_eq!(message.value, Some(serde_json::json!("vvvvvvvvvv")));
        assert!(message.truncated);
        assert_eq!(message.size, 100);
    }

    #[test]
    fn values_are_cut_at_the_byte_limit_without_splitting_characters() {
        assert_eq!(cut(b"abcdef", 10), b"abcdef");
        assert_eq!(cut(b"abcdef", 4), b"abcd");
        assert_eq!(cut("aé".as_bytes(), 2), b"a");
        assert_eq!(cut(&[0xff, 0xfe, 0xfd], 2), &[0xff, 0xfe]);
    }
}
