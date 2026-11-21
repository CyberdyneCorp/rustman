//! One MQTT session: the CONNECT/CONNACK handshake, then packets in and
//! out over a [`Link`], with packet ids for QoS 1 and 2.

use std::time::Duration;

use bytes::BytesMut;

use super::PROTOCOL;
use super::link::Link;
use super::packet::{Codec, Connect, Packet};
use crate::MessagingError;

/// CONNACK refusals that mean the credentials were wrong.
const AUTH_REFUSALS: [&str; 2] = ["BadUserNamePassword", "NotAuthorized"];

pub struct Session {
    link: Link,
    codec: Codec,
    buf: BytesMut,
    last_pkid: u16,
}

impl Session {
    /// Send `connect` over `link` and wait up to `timeout` for the CONNACK;
    /// also returns whether the broker kept a session for the client.
    pub async fn connect(
        link: Link,
        codec: Codec,
        connect: Connect,
        timeout: Duration,
    ) -> Result<(Self, bool), MessagingError> {
        let mut session = Self {
            link,
            codec,
            buf: BytesMut::with_capacity(4096),
            last_pkid: 0,
        };
        session.send(&Packet::Connect(connect)).await?;
        let ack = session
            .expect(timeout, "CONNACK", |p| matches!(p, Packet::ConnAck { .. }))
            .await?;
        let Packet::ConnAck {
            refused,
            session_present,
        } = ack
        else {
            unreachable!("expect returns a CONNACK");
        };
        match refused {
            None => Ok((session, session_present)),
            Some(code) => Err(refusal(&code)),
        }
    }

    /// A fresh packet id (never 0).
    pub fn pkid(&mut self) -> u16 {
        self.last_pkid = self.last_pkid.checked_add(1).unwrap_or(1);
        self.last_pkid
    }

    pub async fn send(&mut self, packet: &Packet) -> Result<(), MessagingError> {
        let mut bytes = BytesMut::new();
        self.codec
            .write(packet, &mut bytes)
            .map_err(|e| MessagingError::Invalid(format!("mqtt: cannot encode a packet: {e}")))?;
        self.link.send(bytes).await.map_err(lost)
    }

    /// The next packet from the broker. Cancel-safe.
    pub async fn recv(&mut self) -> Result<Packet, MessagingError> {
        loop {
            let packet = self.codec.read(&mut self.buf).map_err(|e| {
                MessagingError::broker(PROTOCOL, format!("invalid packet from the broker: {e}"))
            })?;
            if let Some(packet) = packet {
                return Ok(packet);
            }
            if !self.link.recv(&mut self.buf).await.map_err(lost)? {
                return Err(MessagingError::broker(
                    PROTOCOL,
                    "the broker closed the connection",
                ));
            }
        }
    }

    /// Read until a packet `wanted` accepts arrives, within `timeout`;
    /// other packets are dropped. A DISCONNECT from the broker fails.
    pub async fn expect(
        &mut self,
        timeout: Duration,
        what: &str,
        wanted: impl Fn(&Packet) -> bool,
    ) -> Result<Packet, MessagingError> {
        let wait = async {
            loop {
                let packet = self.recv().await?;
                if wanted(&packet) {
                    return Ok(packet);
                }
                if let Packet::Disconnect { reason } = packet {
                    return Err(disconnected(reason.as_deref()));
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.map_err(|_| {
            MessagingError::broker(
                PROTOCOL,
                format!("no {what} within {} ms", timeout.as_millis()),
            )
        })?
    }

    /// Say goodbye; errors are ignored since the operation is over.
    pub async fn disconnect(mut self) {
        let _ = self.send(&Packet::Disconnect { reason: None }).await;
    }
}

/// The error of a CONNACK refusal `code`.
pub fn refusal(code: &str) -> MessagingError {
    if AUTH_REFUSALS.contains(&code) {
        return MessagingError::AuthFailed {
            protocol: PROTOCOL,
            mechanism: "username/password",
        };
    }
    MessagingError::broker(PROTOCOL, format!("connection refused ({code})"))
}

/// The error of a DISCONNECT sent by the broker.
pub fn disconnected(reason: Option<&str>) -> MessagingError {
    let reason = reason.map(|r| format!(" ({r})")).unwrap_or_default();
    MessagingError::broker(PROTOCOL, format!("the broker disconnected{reason}"))
}

fn lost(e: std::io::Error) -> MessagingError {
    MessagingError::broker(PROTOCOL, format!("connection lost: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_their_cause() {
        assert_eq!(
            refusal("BadUserNamePassword").to_string(),
            "mqtt: authentication failed (username/password)"
        );
        assert_eq!(
            refusal("ServerBusy").to_string(),
            "mqtt: connection refused (ServerBusy)"
        );
        assert_eq!(
            disconnected(Some("SessionTakenOver")).to_string(),
            "mqtt: the broker disconnected (SessionTakenOver)"
        );
    }
}
