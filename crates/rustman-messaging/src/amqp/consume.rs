//! AMQP consume. By default a server-named, exclusive, auto-delete queue
//! is bound to `exchange` with `binding_key`, so existing queues are
//! untouched, and it is deleted when the consume ends. A named `queue`
//! (opted into at load time) is read the same way or, with `requeue`,
//! holds every delivery unacked and hands them all back.
//!
//! Every consume uses manual acks with the prefetch capped at the message
//! bound: the broker never pushes (or removes) more messages than are
//! recorded, which also bounds the deliveries held in memory.

use futures_util::StreamExt;
use lapin::message::Delivery;
use lapin::options::{
    BasicConsumeOptions, BasicNackOptions, BasicQosOptions, QueueBindOptions, QueueDeclareOptions,
    QueueDeleteOptions,
};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Channel, Connection};
use rustman_core::vars::ResolvedMessaging;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use super::broker_error;
use crate::payload::Header;
use crate::{ConsumedMessage, MessagingError, MessagingExchange, RunOptions, sleep_until};

/// The binding key of a temporary queue without `binding_key`.
pub const DEFAULT_BINDING_KEY: &str = "#";

/// The prefetch of an unbounded (interactive) consume.
pub const UNBOUNDED_PREFETCH: u16 = 1000;

/// How deliveries are acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumeMode {
    /// Each delivery is acknowledged after it is recorded.
    Ack,
    /// Deliveries stay unacked and are handed back at the end.
    Requeue,
}

/// The mode of `resolved`: only a named queue with `requeue` hands
/// messages back.
pub fn consume_mode(resolved: &ResolvedMessaging) -> ConsumeMode {
    mode_of(resolved.queue.is_some(), &resolved.amqp)
}

fn mode_of(named: bool, amqp: &rustman_core::vars::ResolvedAmqp) -> ConsumeMode {
    if named && amqp.requeue {
        ConsumeMode::Requeue
    } else {
        ConsumeMode::Ack
    }
}

/// The prefetch count: the message bound, so the broker never delivers
/// more than will be recorded.
pub fn prefetch(bounds: Option<crate::Bounds>) -> u16 {
    bounds.map_or(UNBOUNDED_PREFETCH, |b| {
        u16::try_from(b.max_messages.max(1)).unwrap_or(u16::MAX)
    })
}

pub async fn consume(
    connection: &Connection,
    resolved: &ResolvedMessaging,
    mut options: RunOptions,
    exchange: &mut MessagingExchange,
) -> Result<(), MessagingError> {
    let channel = connection
        .create_channel()
        .await
        .map_err(|e| broker_error("cannot open a channel", &e))?;
    let mode = consume_mode(resolved);
    let (queue, temporary) = match &resolved.queue {
        Some(queue) => (queue.clone(), false),
        None => (temporary_queue(&channel, resolved).await?, true),
    };
    exchange.queue = Some(queue.clone());
    channel
        .basic_qos(prefetch(options.bounds), BasicQosOptions::default())
        .await
        .map_err(|e| broker_error("cannot set the prefetch count", &e))?;
    let consume_options = BasicConsumeOptions {
        no_ack: false,
        exclusive: temporary,
        ..BasicConsumeOptions::default()
    };
    let consumer = channel
        .basic_consume(
            queue.as_str().into(),
            "".into(),
            consume_options,
            FieldTable::default(),
        )
        .await
        .map_err(|e| broker_error(&format!("cannot consume from queue '{queue}'"), &e))?;
    if let Some(ready) = options.ready.take() {
        let _ = ready.send(());
    }
    let mut last_tag = None;
    let collected = collect(consumer, mode, resolved, &options, exchange, &mut last_tag).await;
    if let (ConsumeMode::Requeue, Some(tag)) = (mode, last_tag) {
        hand_back(&channel, tag).await;
    }
    if temporary {
        let _ = channel
            .queue_delete(queue.as_str().into(), QueueDeleteOptions::default())
            .await;
    }
    let _ = channel.close(200, "bye".into()).await;
    collected
}

/// Declare a server-named exclusive auto-delete queue bound to the
/// request's exchange; its name.
async fn temporary_queue(
    channel: &Channel,
    resolved: &ResolvedMessaging,
) -> Result<String, MessagingError> {
    let declare = QueueDeclareOptions {
        exclusive: true,
        auto_delete: true,
        ..QueueDeclareOptions::default()
    };
    let queue = channel
        .queue_declare("".into(), declare, FieldTable::default())
        .await
        .map_err(|e| broker_error("cannot declare a temporary queue", &e))?;
    let name = queue.name().as_str().to_owned();
    let target = resolved.exchange.clone().unwrap_or_default();
    let key = resolved
        .binding_key
        .as_deref()
        .unwrap_or(DEFAULT_BINDING_KEY);
    channel
        .queue_bind(
            name.as_str().into(),
            target.as_str().into(),
            key.into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await
        .map_err(|e| broker_error(&format!("cannot bind to exchange '{target}'"), &e))?;
    Ok(name)
}

async fn collect(
    mut consumer: lapin::Consumer,
    mode: ConsumeMode,
    resolved: &ResolvedMessaging,
    options: &RunOptions,
    exchange: &mut MessagingExchange,
    last_tag: &mut Option<u64>,
) -> Result<(), MessagingError> {
    let deadline = options.bounds.map(|b| Instant::now() + b.duration);
    let max = options.bounds.map_or(u64::MAX, |b| b.max_messages);
    let max_bytes =
        usize::try_from(resolved.limits.effective_max_message_bytes()).unwrap_or(usize::MAX);
    while (exchange.messages.len() as u64) < max {
        let delivery = tokio::select! {
            () = options.cancel.cancelled() => return Ok(()),
            () = sleep_until(deadline) => return Ok(()),
            delivery = consumer.next() => delivery,
        };
        let Some(delivery) = delivery else {
            return Ok(());
        };
        let delivery = delivery.map_err(|e| broker_error("consume failed", &e))?;
        if mode == ConsumeMode::Ack {
            delivery
                .acker
                .ack(lapin::options::BasicAckOptions::default())
                .await
                .map_err(|e| broker_error("ack failed", &e))?;
        }
        let message = delivery_message(&delivery, max_bytes);
        if let Some(feed) = &options.feed {
            let _ = feed.send(message.clone());
        }
        exchange.messages.push(message);
        *last_tag = Some(delivery.delivery_tag);
    }
    Ok(())
}

/// Reject every held delivery, up to `tag`, back onto the queue at once.
async fn hand_back(channel: &Channel, tag: u64) {
    let options = BasicNackOptions {
        multiple: true,
        requeue: true,
    };
    let _ = channel.basic_nack(tag, options).await;
}

/// The recorded form of `delivery`.
pub fn delivery_message(delivery: &Delivery, max_bytes: usize) -> ConsumedMessage {
    ConsumedMessage {
        exchange: Some(delivery.exchange.as_str().to_owned()),
        routing_key: Some(delivery.routing_key.as_str().to_owned()),
        redelivered: delivery.redelivered,
        headers: headers(&delivery.properties),
        properties: properties_json(&delivery.properties),
        ..ConsumedMessage::with_value(&delivery.data, max_bytes)
    }
}

fn headers(props: &BasicProperties) -> Vec<Header> {
    let Some(table) = props.headers() else {
        return Vec::new();
    };
    table
        .inner()
        .iter()
        .map(|(name, value)| match value {
            AMQPValue::LongString(text) => Header::from_bytes(name.as_str(), text.as_bytes()),
            other => Header::from_bytes(name.as_str(), field_text(other).as_bytes()),
        })
        .collect()
}

fn field_text(value: &AMQPValue) -> String {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) if map.len() == 1 => map
            .into_iter()
            .next()
            .map(|(_, v)| plain(v))
            .unwrap_or_default(),
        Ok(other) => plain(other),
        Err(_) => format!("{value:?}"),
    }
}

fn plain(value: Value) -> String {
    match value {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

fn properties_json(props: &BasicProperties) -> Option<Value> {
    let mut map = Map::new();
    let text = |v: &Option<lapin::types::ShortString>| v.as_ref().map(|s| json!(s.as_str()));
    let fields = [
        ("content_type", text(props.content_type())),
        ("correlation_id", text(props.correlation_id())),
        ("reply_to", text(props.reply_to())),
        ("expiration", text(props.expiration())),
        ("message_id", text(props.message_id())),
        ("delivery_mode", props.delivery_mode().map(|m| json!(m))),
        ("timestamp", props.timestamp().map(|t| json!(t))),
    ];
    for (name, value) in fields {
        if let Some(value) = value {
            map.insert(name.to_owned(), value);
        }
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use lapin::types::LongString;
    use rustman_core::model::AckMode;
    use rustman_core::vars::ResolvedAmqp;

    use super::*;

    #[test]
    fn named_queues_choose_the_ack_mode() {
        let amqp = |ack, requeue| ResolvedAmqp {
            mandatory: false,
            ack,
            requeue,
        };
        let cases = [
            (None, amqp(AckMode::Auto, false), ConsumeMode::Ack),
            (None, amqp(AckMode::Manual, true), ConsumeMode::Ack),
            (Some("q"), amqp(AckMode::Auto, true), ConsumeMode::Requeue),
            (Some("q"), amqp(AckMode::Auto, false), ConsumeMode::Ack),
            (Some("q"), amqp(AckMode::Manual, false), ConsumeMode::Ack),
            (Some("q"), amqp(AckMode::Manual, true), ConsumeMode::Requeue),
        ];
        for (queue, amqp, mode) in cases {
            assert_eq!(mode_of(queue.is_some(), &amqp), mode);
        }
    }

    #[test]
    fn the_prefetch_never_exceeds_the_message_bound() {
        let bounds = |max_messages| crate::Bounds {
            max_messages,
            duration: std::time::Duration::from_secs(1),
        };
        assert_eq!(prefetch(Some(bounds(2))), 2);
        assert_eq!(prefetch(Some(bounds(0))), 1);
        assert_eq!(prefetch(Some(bounds(1_000_000))), u16::MAX);
        assert_eq!(prefetch(None), UNBOUNDED_PREFETCH);
    }

    #[test]
    fn deliveries_record_routing_headers_and_properties() {
        let mut table = FieldTable::default();
        table.insert(
            "source".into(),
            AMQPValue::LongString(LongString::from("rustman")),
        );
        table.insert("attempt".into(), AMQPValue::LongInt(2));
        let properties = BasicProperties::default()
            .with_content_type("application/json".into())
            .with_delivery_mode(2)
            .with_headers(table);
        let mut delivery = Delivery::mock(
            1,
            "orders".into(),
            "order.created".into(),
            true,
            br#"{"id":1}"#.to_vec(),
        );
        delivery.properties = properties;
        let message = delivery_message(&delivery, 1024);
        assert_eq!(message.exchange.as_deref(), Some("orders"));
        assert_eq!(message.routing_key.as_deref(), Some("order.created"));
        assert!(message.redelivered);
        assert_eq!(message.value, Some(json!({"id": 1})));
        let headers: Vec<_> = message
            .headers
            .iter()
            .map(|h| (h.name.as_str(), h.value.as_str()))
            .collect();
        assert_eq!(headers, [("attempt", "2"), ("source", "rustman")]);
        assert_eq!(
            message.properties,
            Some(json!({"content_type": "application/json", "delivery_mode": 2}))
        );
    }
}
