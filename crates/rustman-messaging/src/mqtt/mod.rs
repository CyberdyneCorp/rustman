//! The MQTT client (messaging-requests "MQTT publish", "MQTT subscribe",
//! "MQTT security"; design D2): MQTT 3.1.1 and 5 packets through
//! `rumqttc`'s codecs over our own TCP, rustls or WebSocket link, so TLS
//! goes through the shared `rustman-http` settings builder. A publish
//! waits for each message's PUBACK or PUBREC/PUBREL/PUBCOMP flow; a
//! subscribe starts a clean session by default.

mod link;
mod packet;
mod session;
mod v4;
mod v5;

use std::sync::Arc;
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use rustman_core::model::Operation;
use rustman_core::vars::ResolvedMessaging;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

pub use link::{Endpoint, Io, Link, Transport, endpoint};
pub use packet::{Codec, Connect, MAX_PACKET, Packet, Publish, PublishProperties};

use session::{Session, disconnected};

use crate::payload::{Header, encode_message, render};
use crate::{
    ConsumedMessage, MessagingError, MessagingExchange, ProducedMessage, RunOptions, sleep_until,
};

pub(crate) const PROTOCOL: &str = "mqtt";

/// The keep-alive asked for; a consume pings at half of it.
const KEEP_ALIVE: Duration = Duration::from_secs(30);

/// How long the broker may take to acknowledge a publish or subscribe.
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the publish or subscribe of a `kind: mqtt` request.
pub async fn run(
    resolved: &ResolvedMessaging,
    mut options: RunOptions,
) -> Result<MessagingExchange, MessagingError> {
    let endpoint = endpoint(resolved.broker.as_deref().unwrap_or_default())?;
    let tls = match endpoint.transport.uses_tls() {
        true => Some(Arc::new(crate::tls::client_config(resolved)?)),
        false => None,
    };
    let link = link::Link::open(&endpoint, tls, options.connect_timeout).await?;
    let client_id = resolved
        .mqtt
        .client_id
        .clone()
        .unwrap_or_else(generated_client_id);
    let consume = resolved.operation == Operation::Consume;
    let connect = Connect {
        client_id: client_id.clone(),
        clean: !consume || resolved.mqtt.clean_session,
        keep_alive: KEEP_ALIVE.as_secs() as u16,
        login: resolved
            .auth
            .as_ref()
            .map(|a| (a.username.clone(), a.password.clone().unwrap_or_default())),
    };
    let codec = Codec(resolved.mqtt.version);
    let (mut session, _) = Session::connect(link, codec, connect, options.connect_timeout).await?;
    let mut exchange = MessagingExchange::start(resolved);
    let result = match resolved.operation {
        Operation::Produce => publish_all(&mut session, resolved, &mut exchange).await,
        Operation::Consume => {
            if !resolved.mqtt.clean_session {
                exchange
                    .warnings
                    .push(persistent_session_warning(&client_id));
            }
            let topic = resolved.topic.clone().unwrap_or_default();
            subscribe(&mut session, &topic, resolved.mqtt.qos.level()).await?;
            if let Some(ready) = options.ready.take() {
                let _ = ready.send(());
            }
            collect(&mut session, resolved, &options, &mut exchange).await
        }
    };
    session.disconnect().await;
    result.map(|()| exchange)
}

/// `rustman-` and 8 random hex digits.
pub fn generated_client_id() -> String {
    let mut bytes = [0u8; 4];
    let _ = SystemRandom::new().fill(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("rustman-{hex}")
}

/// The warning of a subscribe that resumes a persistent session.
pub fn persistent_session_warning(client_id: &str) -> String {
    format!(
        "mqtt: clean_session is false: queued messages for client id '{client_id}' will be consumed from the broker"
    )
}

/// The publish packet of one message.
fn publish_packet(
    resolved: &ResolvedMessaging,
    pkid: u16,
    value: Option<Vec<u8>>,
    headers: &[(String, Vec<u8>)],
) -> Publish {
    let props = &resolved.properties;
    let user = headers
        .iter()
        .map(|(n, v)| (n.clone(), String::from_utf8_lossy(v).into_owned()))
        .collect();
    Publish {
        topic: resolved.topic.clone().unwrap_or_default(),
        qos: resolved.mqtt.qos.level(),
        retain: resolved.mqtt.retain,
        pkid,
        payload: value.unwrap_or_default().into(),
        properties: PublishProperties {
            content_type: props.content_type.clone(),
            correlation_data: props.correlation_id.as_deref().map(v5::correlation),
            response_topic: props.response_topic.clone(),
            message_expiry: props.message_expiry,
            user,
        },
    }
}

async fn publish_all(
    session: &mut Session,
    resolved: &ResolvedMessaging,
    exchange: &mut MessagingExchange,
) -> Result<(), MessagingError> {
    let qos = resolved.mqtt.qos.level();
    for message in &resolved.messages {
        let encoded = encode_message(message).await?;
        let size = encoded.size();
        let pkid = if qos == 0 { 0 } else { session.pkid() };
        let packet = publish_packet(resolved, pkid, encoded.value.clone(), &encoded.headers);
        let topic = packet.topic.clone();
        session.send(&Packet::Publish(packet)).await?;
        let ack = acknowledge(session, qos, pkid, &topic).await?;
        exchange.produced.push(ProducedMessage {
            headers: encoded
                .headers
                .iter()
                .map(|(n, v)| Header::from_bytes(n, v))
                .collect(),
            value: encoded.value.as_deref().map(render),
            size,
            ack,
            ..ProducedMessage::default()
        });
    }
    Ok(())
}

/// Wait for the broker's acknowledgement of publish `pkid` at `qos`: the
/// flow completed, or `None` for QoS 0.
async fn acknowledge(
    session: &mut Session,
    qos: u8,
    pkid: u16,
    topic: &str,
) -> Result<Option<String>, MessagingError> {
    let refused = |reason: String| {
        MessagingError::broker(PROTOCOL, format!("publish to '{topic}' refused ({reason})"))
    };
    match qos {
        0 => Ok(None),
        1 => {
            let ack = session
                .expect(
                    ACK_TIMEOUT,
                    "PUBACK",
                    |p| matches!(p, Packet::PubAck { pkid: id, .. } if *id == pkid),
                )
                .await?;
            match ack {
                Packet::PubAck {
                    refused: Some(r), ..
                } => Err(refused(r)),
                _ => Ok(Some("PUBACK".to_owned())),
            }
        }
        _ => {
            let rec = session
                .expect(
                    ACK_TIMEOUT,
                    "PUBREC",
                    |p| matches!(p, Packet::PubRec { pkid: id, .. } if *id == pkid),
                )
                .await?;
            if let Packet::PubRec {
                refused: Some(r), ..
            } = rec
            {
                return Err(refused(r));
            }
            session.send(&Packet::PubRel { pkid }).await?;
            session
                .expect(
                    ACK_TIMEOUT,
                    "PUBCOMP",
                    |p| matches!(p, Packet::PubComp { pkid: id } if *id == pkid),
                )
                .await?;
            Ok(Some("PUBREC/PUBREL/PUBCOMP".to_owned()))
        }
    }
}

async fn subscribe(session: &mut Session, filter: &str, qos: u8) -> Result<(), MessagingError> {
    let pkid = session.pkid();
    let subscribe = Packet::Subscribe {
        pkid,
        filter: filter.to_owned(),
        qos,
    };
    session.send(&subscribe).await?;
    let ack = session
        .expect(
            ACK_TIMEOUT,
            "SUBACK",
            |p| matches!(p, Packet::SubAck { pkid: id, .. } if *id == pkid),
        )
        .await?;
    match ack {
        Packet::SubAck {
            refused: Some(reason),
            ..
        } => Err(MessagingError::broker(
            PROTOCOL,
            format!("subscription to '{filter}' refused ({reason})"),
        )),
        _ => Ok(()),
    }
}

/// Record received messages until the bounds, the cancel token, or the
/// broker ends the session.
async fn collect(
    session: &mut Session,
    resolved: &ResolvedMessaging,
    options: &RunOptions,
    exchange: &mut MessagingExchange,
) -> Result<(), MessagingError> {
    let deadline = options.bounds.map(|b| Instant::now() + b.duration);
    let max = options.bounds.map_or(u64::MAX, |b| b.max_messages);
    let max_bytes =
        usize::try_from(resolved.limits.effective_max_message_bytes()).unwrap_or(usize::MAX);
    let mut ping = tokio::time::interval(KEEP_ALIVE / 2);
    ping.reset();
    while exchange.received() < max {
        let packet = tokio::select! {
            () = options.cancel.cancelled() => return Ok(()),
            () = sleep_until(deadline) => return Ok(()),
            _ = ping.tick() => {
                session.send(&Packet::PingReq).await?;
                continue;
            }
            packet = session.recv() => packet?,
        };
        if let Some(message) = handle(session, packet, max_bytes).await? {
            if let Some(feed) = &options.feed {
                let _ = feed.send(message.clone());
            }
            exchange.record(message, options.bounds.is_none());
        }
    }
    Ok(())
}

/// Answer `packet` as the protocol requires; a publish becomes a message.
async fn handle(
    session: &mut Session,
    packet: Packet,
    max_bytes: usize,
) -> Result<Option<ConsumedMessage>, MessagingError> {
    match packet {
        Packet::Publish(publish) => {
            match publish.qos {
                1 => {
                    session
                        .send(&Packet::PubAck {
                            pkid: publish.pkid,
                            refused: None,
                        })
                        .await?
                }
                2 => {
                    session
                        .send(&Packet::PubRec {
                            pkid: publish.pkid,
                            refused: None,
                        })
                        .await?
                }
                _ => {}
            }
            Ok(Some(received(publish, max_bytes)))
        }
        Packet::PubRel { pkid } => {
            session.send(&Packet::PubComp { pkid }).await?;
            Ok(None)
        }
        Packet::Disconnect { reason } => Err(disconnected(reason.as_deref())),
        _ => Ok(None),
    }
}

/// The recorded form of a received publish.
pub fn received(publish: Publish, max_bytes: usize) -> ConsumedMessage {
    let properties = properties_json(&publish.properties);
    ConsumedMessage {
        topic: Some(publish.topic),
        qos: Some(publish.qos),
        retain: publish.retain,
        headers: publish
            .properties
            .user
            .iter()
            .map(|(n, v)| Header::from_bytes(n, v.as_bytes()))
            .collect(),
        properties,
        ..ConsumedMessage::with_value(&publish.payload, max_bytes)
    }
}

fn properties_json(props: &PublishProperties) -> Option<Value> {
    let mut map = Map::new();
    let mut put = |name: &str, value: Option<Value>| {
        if let Some(value) = value {
            map.insert(name.to_owned(), value);
        }
    };
    put("content_type", props.content_type.clone().map(Value::from));
    put(
        "correlation_id",
        props.correlation_data.as_deref().map(render),
    );
    put(
        "response_topic",
        props.response_topic.clone().map(Value::from),
    );
    put("message_expiry", props.message_expiry.map(|s| json!(s)));
    (!map.is_empty()).then_some(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn generated_client_ids_are_random_hex() {
        let id = generated_client_id();
        assert_eq!(id.len(), "rustman-".len() + 8);
        assert!(id.starts_with("rustman-"));
        assert!(id[8..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, generated_client_id());
    }

    #[test]
    fn received_publishes_keep_topic_qos_retain_and_properties() {
        let publish = Publish {
            topic: "sensors/room1/temp".into(),
            qos: 1,
            retain: true,
            pkid: 1,
            payload: Bytes::from_static(br#"{"c":21.5}"#),
            properties: PublishProperties {
                content_type: Some("application/json".into()),
                user: vec![("unit".into(), "C".into())],
                ..PublishProperties::default()
            },
        };
        let message = received(publish, 4);
        assert_eq!(message.topic.as_deref(), Some("sensors/room1/temp"));
        assert_eq!((message.qos, message.retain), (Some(1), true));
        assert_eq!(message.headers[0].name, "unit");
        assert_eq!(
            message.properties,
            Some(json!({"content_type": "application/json"}))
        );
        assert_eq!((message.size, message.truncated), (10, true));
        assert_eq!(message.value, Some(json!("{\"c\"")));
    }

    #[test]
    fn the_persistent_session_warning_names_the_client() {
        assert_eq!(
            persistent_session_warning("probe-1"),
            "mqtt: clean_session is false: queued messages for client id 'probe-1' will be consumed from the broker"
        );
    }
}
