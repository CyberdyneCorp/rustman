//! AMQP publish with publisher confirms: every message waits for the
//! broker's confirm; a nack is `amqp: message not confirmed` and a
//! mandatory message returned as unroutable is `amqp: unroutable`.

use lapin::options::{BasicPublishOptions, ConfirmSelectOptions};
use lapin::types::{AMQPValue, FieldTable, LongString};
use lapin::{BasicProperties, Confirmation, Connection};
use rustman_core::model::MessageProperties;
use rustman_core::vars::ResolvedMessaging;

use super::{PROTOCOL, broker_error};
use crate::payload::{Header, encode_message, render};
use crate::{MessagingError, MessagingExchange, ProducedMessage};

pub async fn publish_all(
    connection: &Connection,
    resolved: &ResolvedMessaging,
    exchange: &mut MessagingExchange,
) -> Result<(), MessagingError> {
    let channel = connection
        .create_channel()
        .await
        .map_err(|e| broker_error("cannot open a channel", &e))?;
    channel
        .confirm_select(ConfirmSelectOptions::default())
        .await
        .map_err(|e| broker_error("cannot enable publisher confirms", &e))?;
    let target = resolved.exchange.clone().unwrap_or_default();
    let routing_key = resolved.routing_key.clone().unwrap_or_default();
    let options = BasicPublishOptions {
        mandatory: resolved.amqp.mandatory,
        ..BasicPublishOptions::default()
    };
    for message in &resolved.messages {
        let encoded = encode_message(message).await?;
        let value = encoded.value.clone().unwrap_or_default();
        let props = properties(&resolved.properties, &encoded.headers);
        let confirm = channel
            .basic_publish(
                target.as_str().into(),
                routing_key.as_str().into(),
                options,
                &value,
                props,
            )
            .await
            .map_err(|e| broker_error("publish failed", &e))?;
        let confirmation = confirm
            .await
            .map_err(|e| broker_error("publish failed", &e))?;
        if let Some(error) = confirmation_error(&confirmation) {
            return Err(error);
        }
        exchange.produced.push(ProducedMessage {
            headers: encoded
                .headers
                .iter()
                .map(|(n, v)| Header::from_bytes(n, v))
                .collect(),
            value: encoded.value.as_deref().map(render),
            size: encoded.size(),
            ack: Some("confirm".to_owned()),
            ..ProducedMessage::default()
        });
    }
    Ok(())
}

/// The error a publisher confirm stands for, if any.
pub fn confirmation_error(confirmation: &Confirmation) -> Option<MessagingError> {
    match confirmation {
        Confirmation::Ack(Some(_)) => Some(MessagingError::broker(PROTOCOL, "unroutable")),
        Confirmation::Nack(_) => Some(MessagingError::broker(PROTOCOL, "message not confirmed")),
        Confirmation::Ack(None) | Confirmation::NotRequested => None,
    }
}

/// The AMQP properties of a message: the request's `properties` and the
/// message headers as a header table.
pub fn properties(props: &MessageProperties, headers: &[(String, Vec<u8>)]) -> BasicProperties {
    let mut basic = BasicProperties::default();
    if let Some(v) = &props.content_type {
        basic = basic.with_content_type(v.as_str().into());
    }
    if let Some(v) = &props.correlation_id {
        basic = basic.with_correlation_id(v.as_str().into());
    }
    if let Some(v) = &props.reply_to {
        basic = basic.with_reply_to(v.as_str().into());
    }
    if let Some(v) = props.delivery_mode {
        basic = basic.with_delivery_mode(v);
    }
    if let Some(v) = props.expiration {
        basic = basic.with_expiration(v.to_string().into());
    }
    if !headers.is_empty() {
        let mut table = FieldTable::default();
        for (name, value) in headers {
            table.insert(
                name.as_str().into(),
                AMQPValue::LongString(LongString::from(value.clone())),
            );
        }
        basic = basic.with_headers(table);
    }
    basic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirms_map_to_errors() {
        assert!(confirmation_error(&Confirmation::Ack(None)).is_none());
        assert!(confirmation_error(&Confirmation::NotRequested).is_none());
        assert_eq!(
            confirmation_error(&Confirmation::Nack(None))
                .unwrap()
                .to_string(),
            "amqp: message not confirmed"
        );
    }

    #[test]
    fn properties_and_headers_are_set() {
        let props = MessageProperties {
            content_type: Some("application/json".into()),
            correlation_id: Some("c-1".into()),
            reply_to: Some("replies".into()),
            delivery_mode: Some(2),
            expiration: Some(60_000),
            ..MessageProperties::default()
        };
        let basic = properties(&props, &[("source".into(), b"rustman".to_vec())]);
        assert_eq!(
            basic.content_type().as_ref().map(|v| v.as_str()),
            Some("application/json")
        );
        assert_eq!(*basic.delivery_mode(), Some(2));
        assert_eq!(
            basic.expiration().as_ref().map(|v| v.as_str()),
            Some("60000")
        );
        let headers = basic.headers().as_ref().unwrap();
        assert!(headers.inner().contains_key("source"));
        assert!(
            properties(&MessageProperties::default(), &[])
                .headers()
                .is_none()
        );
    }
}
