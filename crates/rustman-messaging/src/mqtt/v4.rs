//! MQTT 3.1.1 packets through `rumqttc`'s v4 codec.

use bytes::BytesMut;
use rumqttc::mqttbytes::v4::{self as wire, ConnectReturnCode, SubscribeReasonCode};
use rumqttc::mqttbytes::{Error, QoS};

use super::packet::{Connect, MAX_PACKET, Packet, Publish, PublishProperties};

pub fn qos(level: u8) -> QoS {
    match level {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

pub fn write(packet: &Packet, buf: &mut BytesMut) -> Result<(), String> {
    let wire = to_wire(packet).ok_or("this packet has no MQTT 3.1.1 form")?;
    wire.write(buf, MAX_PACKET)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn read(buf: &mut BytesMut) -> Result<Option<Packet>, String> {
    match wire::Packet::read(buf, MAX_PACKET) {
        Ok(packet) => Ok(Some(from_wire(packet))),
        Err(Error::InsufficientBytes(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn to_wire(packet: &Packet) -> Option<wire::Packet> {
    let wire = match packet {
        Packet::Connect(c) => wire::Packet::Connect(connect(c)),
        Packet::ConnAck {
            refused,
            session_present,
        } => {
            let code = match refused {
                None => ConnectReturnCode::Success,
                Some(_) => ConnectReturnCode::NotAuthorized,
            };
            wire::Packet::ConnAck(wire::ConnAck::new(code, *session_present))
        }
        Packet::Publish(p) => wire::Packet::Publish(publish(p)),
        Packet::PubAck { pkid, .. } => wire::Packet::PubAck(wire::PubAck::new(*pkid)),
        Packet::PubRec { pkid, .. } => wire::Packet::PubRec(wire::PubRec::new(*pkid)),
        Packet::PubRel { pkid } => wire::Packet::PubRel(wire::PubRel::new(*pkid)),
        Packet::PubComp { pkid } => wire::Packet::PubComp(wire::PubComp::new(*pkid)),
        Packet::Subscribe { pkid, filter, qos } => {
            let mut subscribe = wire::Subscribe::new(filter.clone(), self::qos(*qos));
            subscribe.pkid = *pkid;
            wire::Packet::Subscribe(subscribe)
        }
        Packet::SubAck { pkid, refused } => {
            let code = match refused {
                None => SubscribeReasonCode::Success(QoS::AtLeastOnce),
                Some(_) => SubscribeReasonCode::Failure,
            };
            wire::Packet::SubAck(wire::SubAck::new(*pkid, vec![code]))
        }
        Packet::PingReq => wire::Packet::PingReq,
        Packet::PingResp => wire::Packet::PingResp,
        Packet::Disconnect { .. } => wire::Packet::Disconnect,
        Packet::Other => return None,
    };
    Some(wire)
}

fn connect(c: &Connect) -> wire::Connect {
    let mut connect = wire::Connect::new(c.client_id.clone());
    connect.keep_alive = c.keep_alive;
    connect.clean_session = c.clean;
    if let Some((user, password)) = &c.login {
        connect.set_login(user.clone(), password.clone());
    }
    connect
}

fn publish(p: &Publish) -> wire::Publish {
    let mut publish = wire::Publish::from_bytes(p.topic.clone(), qos(p.qos), p.payload.clone());
    publish.retain = p.retain;
    publish.pkid = p.pkid;
    publish
}

fn from_wire(packet: wire::Packet) -> Packet {
    match packet {
        wire::Packet::Connect(c) => Packet::Connect(Connect {
            client_id: c.client_id,
            clean: c.clean_session,
            keep_alive: c.keep_alive,
            login: c.login.map(|l| (l.username, l.password)),
        }),
        wire::Packet::ConnAck(ack) => Packet::ConnAck {
            refused: (ack.code != ConnectReturnCode::Success).then(|| format!("{:?}", ack.code)),
            session_present: ack.session_present,
        },
        wire::Packet::Publish(p) => Packet::Publish(Publish {
            topic: p.topic,
            qos: p.qos as u8,
            retain: p.retain,
            pkid: p.pkid,
            payload: p.payload,
            properties: PublishProperties::default(),
        }),
        wire::Packet::PubAck(a) => Packet::PubAck {
            pkid: a.pkid,
            refused: None,
        },
        wire::Packet::PubRec(a) => Packet::PubRec {
            pkid: a.pkid,
            refused: None,
        },
        wire::Packet::PubRel(a) => Packet::PubRel { pkid: a.pkid },
        wire::Packet::PubComp(a) => Packet::PubComp { pkid: a.pkid },
        other => from_wire_rest(other),
    }
}

fn from_wire_rest(packet: wire::Packet) -> Packet {
    match packet {
        wire::Packet::Subscribe(s) => {
            let first = s.filters.into_iter().next();
            Packet::Subscribe {
                pkid: s.pkid,
                qos: first.as_ref().map_or(0, |f| f.qos as u8),
                filter: first.map(|f| f.path).unwrap_or_default(),
            }
        }
        wire::Packet::SubAck(ack) => Packet::SubAck {
            pkid: ack.pkid,
            refused: ack
                .return_codes
                .contains(&SubscribeReasonCode::Failure)
                .then(|| "Failure".to_owned()),
        },
        wire::Packet::PingReq => Packet::PingReq,
        wire::Packet::PingResp => Packet::PingResp,
        wire::Packet::Disconnect => Packet::Disconnect { reason: None },
        _ => Packet::Other,
    }
}
