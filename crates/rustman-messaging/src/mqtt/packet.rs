//! MQTT packets as the client uses them, the same for 3.1.1 and 5. The
//! `v4` and `v5` modules turn them into bytes with `rumqttc`'s codecs.

use bytes::{Bytes, BytesMut};
use rustman_core::model::MqttVersion;

use super::{v4, v5};

/// The largest packet read or written (MQTT's own limit is 256 MiB).
pub const MAX_PACKET: usize = 64 * 1024 * 1024;

/// The MQTT 5 properties of a publish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishProperties {
    pub content_type: Option<String>,
    pub correlation_data: Option<Bytes>,
    pub response_topic: Option<String>,
    pub message_expiry: Option<u32>,
    pub user: Vec<(String, String)>,
}

impl PublishProperties {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publish {
    pub topic: String,
    pub qos: u8,
    pub retain: bool,
    pub pkid: u16,
    pub payload: Bytes,
    pub properties: PublishProperties,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connect {
    pub client_id: String,
    pub clean: bool,
    pub keep_alive: u16,
    pub login: Option<(String, String)>,
}

/// A packet; failures carry the broker's reason as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Connect(Connect),
    ConnAck {
        refused: Option<String>,
        session_present: bool,
    },
    Publish(Publish),
    PubAck {
        pkid: u16,
        refused: Option<String>,
    },
    PubRec {
        pkid: u16,
        refused: Option<String>,
    },
    PubRel {
        pkid: u16,
    },
    PubComp {
        pkid: u16,
    },
    Subscribe {
        pkid: u16,
        filter: String,
        qos: u8,
    },
    SubAck {
        pkid: u16,
        refused: Option<String>,
    },
    PingReq,
    PingResp,
    Disconnect {
        reason: Option<String>,
    },
    /// A packet the client does not act on.
    Other,
}

/// The codec of a protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Codec(pub MqttVersion);

impl Codec {
    /// Append `packet` to `buf`.
    pub fn write(self, packet: &Packet, buf: &mut BytesMut) -> Result<(), String> {
        match self.0 {
            MqttVersion::V311 => v4::write(packet, buf),
            MqttVersion::V5 => v5::write(packet, buf),
        }
    }

    /// The next whole packet at the start of `buf`, removed from it;
    /// `None` until enough bytes have arrived.
    pub fn read(self, buf: &mut BytesMut) -> Result<Option<Packet>, String> {
        match self.0 {
            MqttVersion::V311 => v4::read(buf),
            MqttVersion::V5 => v5::read(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(version: MqttVersion, packet: Packet) -> Packet {
        let codec = Codec(version);
        let mut buf = BytesMut::new();
        codec.write(&packet, &mut buf).unwrap();
        let mut partial = buf.split_to(buf.len() - 1);
        assert_eq!(codec.read(&mut partial).unwrap(), None);
        partial.unsplit(buf);
        codec.read(&mut partial).unwrap().unwrap()
    }

    fn publish(properties: PublishProperties) -> Packet {
        Packet::Publish(Publish {
            topic: "sensors/room1/temp".into(),
            qos: 2,
            retain: true,
            pkid: 7,
            payload: Bytes::from_static(b"21.5"),
            properties,
        })
    }

    #[test]
    fn packets_survive_both_codecs() {
        for version in [MqttVersion::V311, MqttVersion::V5] {
            for packet in [
                publish(PublishProperties::default()),
                Packet::PubAck {
                    pkid: 3,
                    refused: None,
                },
                Packet::PubRec {
                    pkid: 3,
                    refused: None,
                },
                Packet::PubRel { pkid: 3 },
                Packet::PubComp { pkid: 3 },
                Packet::Subscribe {
                    pkid: 4,
                    filter: "sensors/+/temp".into(),
                    qos: 1,
                },
                Packet::SubAck {
                    pkid: 4,
                    refused: None,
                },
                Packet::PingReq,
                Packet::PingResp,
                Packet::ConnAck {
                    refused: None,
                    session_present: false,
                },
            ] {
                assert_eq!(round_trip(version, packet.clone()), packet, "{version:?}");
            }
        }
    }

    #[test]
    fn v5_carries_publish_properties() {
        let properties = PublishProperties {
            content_type: Some("application/json".into()),
            correlation_data: Some(Bytes::from_static(b"c-1")),
            response_topic: Some("replies/1".into()),
            message_expiry: Some(60),
            user: vec![("source".into(), "rustman".into())],
        };
        let packet = publish(properties);
        assert_eq!(round_trip(MqttVersion::V5, packet.clone()), packet);
    }

    #[test]
    fn connects_carry_the_login() {
        for version in [MqttVersion::V311, MqttVersion::V5] {
            let connect = Packet::Connect(Connect {
                client_id: "rustman-0a1b2c3d".into(),
                clean: true,
                keep_alive: 30,
                login: Some(("app".into(), "pw".into())),
            });
            assert_eq!(round_trip(version, connect.clone()), connect);
        }
    }
}
