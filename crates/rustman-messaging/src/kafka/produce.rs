//! Kafka produce: each message goes to its partition in file order, and
//! the exchange records the partition and offset the broker assigned.

use std::collections::{BTreeMap, HashMap};

use rskafka::chrono::{DateTime, Utc};
use rskafka::client::Client;
use rskafka::client::partition::{Compression, PartitionClient, UnknownTopicHandling};
use rskafka::record::Record;
use rustman_core::model::Acks;
use rustman_core::vars::ResolvedMessaging;
use serde_json::Value;

use super::partitioner::Partitioner;
use super::{check_partitions, request_error, topic_partitions};
use crate::payload::{EncodedMessage, Header, encode_message, render};
use crate::registry::RegistryClient;
use crate::{MessagingError, MessagingExchange, ProducedMessage};

/// Produce every message of `resolved`, recording each in `exchange`.
pub async fn produce(
    client: &Client,
    resolved: &ResolvedMessaging,
    registry: Option<&RegistryClient>,
    exchange: &mut MessagingExchange,
) -> Result<(), MessagingError> {
    let topic = resolved.topic.clone().unwrap_or_default();
    let partitions = topic_partitions(client, &topic).await?;
    if let Some(partition) = resolved.partition {
        check_partitions(&topic, &partitions, &[partition])?;
    }
    exchange.warnings.extend(acks_warning(resolved.acks));
    let mut partitioner = Partitioner::new(partitions, resolved.partition);
    let mut clients: HashMap<i32, PartitionClient> = HashMap::new();
    for message in &resolved.messages {
        let mut encoded = encode_message(message).await?;
        let schema_id = encode_with_registry(registry, &mut encoded).await?;
        exchange.warnings.extend(duplicate_headers(&encoded));
        let partition = partitioner.choose(encoded.key.as_deref());
        if let std::collections::hash_map::Entry::Vacant(slot) = clients.entry(partition) {
            let partition_client = client
                .partition_client(topic.clone(), partition, UnknownTopicHandling::Error)
                .await
                .map_err(|e| request_error(e, &topic))?;
            slot.insert(partition_client);
        }
        let record = record(&encoded);
        let offsets = clients[&partition]
            .produce(vec![record], Compression::NoCompression)
            .await
            .map_err(|e| request_error(e, &topic))?;
        exchange.produced.push(produced(
            &encoded,
            partition,
            offsets.first().copied(),
            schema_id,
        ));
    }
    Ok(())
}

/// rskafka always asks for `acks: all`; a weaker setting is a warning,
/// not an error, since all is the stronger guarantee.
pub fn acks_warning(acks: Acks) -> Option<String> {
    (acks != Acks::All).then(|| {
        format!(
            "kafka: acks: {} is sent as acks: all (the client waits for every in-sync replica)",
            acks.as_str()
        )
    })
}

/// Kafka keeps one value per header name through rskafka; repeated
/// names keep the last value.
pub fn duplicate_headers(message: &EncodedMessage) -> Vec<String> {
    let mut seen = Vec::new();
    let mut warnings = Vec::new();
    for (name, _) in &message.headers {
        if seen.contains(&name) {
            warnings.push(format!(
                "kafka: header '{name}' is repeated; the last value is sent"
            ));
        } else {
            seen.push(name);
        }
    }
    warnings
}

/// Replace the value with its Schema Registry encoding when a subject is
/// configured, returning the schema id.
async fn encode_with_registry(
    registry: Option<&RegistryClient>,
    message: &mut EncodedMessage,
) -> Result<Option<u32>, MessagingError> {
    let (Some(registry), Some(value)) = (registry, &message.value) else {
        return Ok(None);
    };
    let json: Value = serde_json::from_slice(value).map_err(|e| {
        MessagingError::Invalid(format!("schema registry: the value is not JSON: {e}"))
    })?;
    match registry.encode(&json).await {
        Some(encoded) => {
            let (id, bytes) = encoded?;
            message.value = Some(bytes);
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

/// The rskafka record of `message`, timestamped now.
pub fn record(message: &EncodedMessage) -> Record {
    let headers: BTreeMap<String, Vec<u8>> = message.headers.iter().cloned().collect();
    Record {
        key: message.key.clone(),
        value: message.value.clone(),
        headers,
        timestamp: now(),
    }
}

fn now() -> DateTime<Utc> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    DateTime::from_timestamp_millis(millis).unwrap_or_default()
}

fn produced(
    message: &EncodedMessage,
    partition: i32,
    offset: Option<i64>,
    schema_id: Option<u32>,
) -> ProducedMessage {
    ProducedMessage {
        partition: Some(partition),
        offset,
        key: message.key.as_deref().map(render),
        headers: message
            .headers
            .iter()
            .map(|(name, value)| Header::from_bytes(name, value))
            .collect(),
        value: message.value.as_deref().map(render),
        size: message.size(),
        schema_id,
        ..ProducedMessage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> EncodedMessage {
        EncodedMessage {
            key: Some(b"order-1".to_vec()),
            value: Some(br#"{"id":1,"total":9.5}"#.to_vec()),
            headers: vec![("source".into(), b"rustman".to_vec())],
        }
    }

    #[test]
    fn records_carry_key_value_headers_and_a_timestamp() {
        let record = record(&message());
        assert_eq!(record.key.as_deref(), Some(&b"order-1"[..]));
        assert_eq!(
            record.value.as_deref(),
            Some(&br#"{"id":1,"total":9.5}"#[..])
        );
        assert_eq!(
            record.headers.get("source").map(Vec::as_slice),
            Some(&b"rustman"[..])
        );
        assert!(record.timestamp.timestamp_millis() > 1_700_000_000_000);
    }

    #[test]
    fn the_exchange_records_partition_offset_and_rendered_values() {
        let produced = produced(&message(), 0, Some(41), None);
        assert_eq!((produced.partition, produced.offset), (Some(0), Some(41)));
        assert_eq!(produced.key, Some(Value::String("order-1".into())));
        assert_eq!(
            produced.value,
            Some(serde_json::json!({"id": 1, "total": 9.5}))
        );
        assert_eq!(produced.headers[0].value, "rustman");
    }

    #[test]
    fn weaker_acks_and_repeated_headers_are_warnings() {
        assert_eq!(acks_warning(Acks::All), None);
        let warning = acks_warning(Acks::Leader).unwrap();
        assert!(
            warning.starts_with("kafka: acks: leader is sent as acks: all"),
            "{warning}"
        );
        let mut repeated = message();
        repeated.headers.push(("source".into(), b"other".to_vec()));
        assert_eq!(
            duplicate_headers(&repeated),
            ["kafka: header 'source' is repeated; the last value is sent"]
        );
        assert!(duplicate_headers(&message()).is_empty());
    }
}
