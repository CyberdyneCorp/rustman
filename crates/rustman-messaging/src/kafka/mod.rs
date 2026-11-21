//! The Kafka client (messaging-requests "Kafka produce", "Kafka consume",
//! "Kafka security"; design D1) on `rskafka`: partition-level produce and
//! fetch at explicit offsets, with no consumer group and no offset
//! commits.

mod connect;
mod consume;
mod partitioner;
mod produce;

use rskafka::client::Client;
use rskafka::client::error::{Error as KafkaError, ProtocolError};
use rustman_core::model::Operation;
use rustman_core::vars::ResolvedMessaging;

pub use connect::{
    FRAME_OVERHEAD, bootstrap, connect_error, max_frame_bytes, sasl_config, uses_tls,
};
pub use consume::{QUEUE_BYTES, queue_capacity};
pub use consume::{parse_timestamp, timestamp_text};
pub use partitioner::{Partitioner, murmur2, partition_for_key};

use crate::registry::RegistryClient;
use crate::{MessagingError, MessagingExchange, RunOptions};

pub(crate) const PROTOCOL: &str = "kafka";

/// Run the produce or consume of a `kind: kafka` request.
pub async fn run(
    resolved: &ResolvedMessaging,
    options: RunOptions,
) -> Result<MessagingExchange, MessagingError> {
    let registry = resolved
        .schema_registry
        .as_ref()
        .map(RegistryClient::new)
        .transpose()?;
    let client = connect::connect(resolved, options.connect_timeout).await?;
    let mut exchange = MessagingExchange::start(resolved);
    match resolved.operation {
        Operation::Produce => {
            produce::produce(&client, resolved, registry.as_ref(), &mut exchange).await?;
        }
        Operation::Consume => {
            let run = consume::Consume {
                client: &client,
                resolved,
                registry: registry.as_ref(),
            };
            run.run(options, &mut exchange).await?;
        }
    }
    Ok(exchange)
}

/// The partition ids of `topic`, ascending.
async fn topic_partitions(client: &Client, topic: &str) -> Result<Vec<i32>, MessagingError> {
    let topics = client
        .list_topics()
        .await
        .map_err(|e| request_error(e, topic))?;
    let found = topics.into_iter().find(|t| t.name == topic);
    let partitions = found.ok_or_else(|| MessagingError::UnknownTopic(topic.to_owned()))?;
    Ok(partitions.partitions.into_iter().collect())
}

/// Fail unless every one of `wanted` is a partition of `topic`.
fn check_partitions(topic: &str, existing: &[i32], wanted: &[i32]) -> Result<(), MessagingError> {
    match wanted.iter().find(|p| !existing.contains(p)) {
        Some(p) => Err(MessagingError::Invalid(format!(
            "kafka: topic '{topic}' has no partition {p}"
        ))),
        None => Ok(()),
    }
}

/// The error of a failed request on `topic`.
fn request_error(error: KafkaError, topic: &str) -> MessagingError {
    match &error {
        KafkaError::ServerError {
            protocol_error: ProtocolError::UnknownTopicOrPartition,
            ..
        } => MessagingError::UnknownTopic(topic.to_owned()),
        _ => MessagingError::broker(PROTOCOL, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_partitions_must_exist() {
        assert!(check_partitions("orders", &[0, 1], &[1]).is_ok());
        let err = check_partitions("orders", &[0, 1], &[0, 5]).unwrap_err();
        assert_eq!(err.to_string(), "kafka: topic 'orders' has no partition 5");
    }

    #[test]
    fn unknown_topics_are_named() {
        let err = request_error(KafkaError::InvalidResponse("bad frame".into()), "orders");
        assert_eq!(err.to_string(), "kafka: Invalid response: bad frame");
        assert_eq!(
            MessagingError::UnknownTopic("orders".into()).to_string(),
            "kafka: unknown topic 'orders'"
        );
    }
}
