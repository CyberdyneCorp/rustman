//! MQTT 5 packets through `rumqttc`'s v5 codec.

use bytes::{Bytes, BytesMut};
use rumqttc::v5::mqttbytes::v5::{
    self as wire, ConnAck, ConnectReturnCode, DisconnectReasonCode, Filter, Login, PubAckReason,
    PubRecReason, SubscribeReasonCode,
};
use rumqttc::v5::mqttbytes::{Error, QoS};

use super::packet::{Connect, MAX_PACKET, Packet, Publish, PublishProperties};

const MAX: Option<u32> = Some(MAX_PACKET as u32);

fn qos(level: u8) -> QoS {
    match level {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

pub fn write(packet: &Packet, buf: &mut BytesMut) -> Result<(), String> {
    let wire = to_wire(packet).ok_or("this packet has no MQTT 5 form")?;
    wire.write(buf, MAX).map(|_| ()).map_err(|e| e.to_string())
}

pub fn read(buf: &mut BytesMut) -> Result<Option<Packet>, String> {
    match wire::Packet::read(buf, MAX) {
        Ok(packet) => Ok(Some(from_wire(packet))),
        Err(Error::InsufficientBytes(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn to_wire(packet: &Packet) -> Option<wire::Packet> {
    let wire = match packet {
        Packet::Connect(c) => connect(c),
        Packet::ConnAck {
            refused,
            session_present,
        } => wire::Packet::ConnAck(ConnAck {
            session_present: *session_present,
            code: match refused {
                None => ConnectReturnCode::Success,
                Some(_) => ConnectReturnCode::NotAuthorized,
            },
            properties: None,
        }),
        Packet::Publish(p) => wire::Packet::Publish(publish(p)),
        Packet::PubAck { pkid, refused } => {
            let mut ack = wire::PubAck::new(*pkid, None);
            if refused.is_some() {
                ack.reason = PubAckReason::NotAuthorized;
            }
            wire::Packet::PubAck(ack)
        }
        Packet::PubRec { pkid, refused } => {
            let mut rec = wire::PubRec::new(*pkid, None);
            if refused.is_some() {
                rec.reason = PubRecReason::NotAuthorized;
            }
            wire::Packet::PubRec(rec)
        }
        Packet::PubRel { pkid } => wire::Packet::PubRel(wire::PubRel::new(*pkid, None)),
        Packet::PubComp { pkid } => wire::Packet::PubComp(wire::PubComp::new(*pkid, None)),
        Packet::Subscribe { pkid, filter, qos } => {
            let mut subscribe =
                wire::Subscribe::new(Filter::new(filter.clone(), self::qos(*qos)), None);
            subscribe.pkid = *pkid;
            wire::Packet::Subscribe(subscribe)
        }
        Packet::SubAck { pkid, refused } => wire::Packet::SubAck(wire::SubAck {
            pkid: *pkid,
            return_codes: vec![match refused {
                None => SubscribeReasonCode::Success(QoS::AtLeastOnce),
                Some(_) => SubscribeReasonCode::NotAuthorized,
            }],
            properties: None,
        }),
        Packet::PingReq => wire::Packet::PingReq(wire::PingReq),
        Packet::PingResp => wire::Packet::PingResp(wire::PingResp),
        Packet::Disconnect { .. } => wire::Packet::Disconnect(wire::Disconnect::new(
            DisconnectReasonCode::NormalDisconnection,
        )),
        Packet::Other => return None,
    };
    Some(wire)
}

fn connect(c: &Connect) -> wire::Packet {
    let connect = wire::Connect {
        keep_alive: c.keep_alive,
        client_id: c.client_id.clone(),
        clean_start: c.clean,
        properties: None,
    };
    let login = c
        .login
        .as_ref()
        .map(|(u, p)| Login::new(u.clone(), p.clone()));
    wire::Packet::Connect(connect, None, login)
}

fn publish(p: &Publish) -> wire::Publish {
    let properties = (!p.properties.is_empty()).then(|| {
        let props = &p.properties;
        wire::PublishProperties {
            message_expiry_interval: props.message_expiry,
            response_topic: props.response_topic.clone(),
            correlation_data: props.correlation_data.clone(),
            user_properties: props.user.clone(),
            content_type: props.content_type.clone(),
            ..wire::PublishProperties::default()
        }
    });
    let mut publish =
        wire::Publish::new(p.topic.clone(), qos(p.qos), p.payload.clone(), properties);
    publish.retain = p.retain;
    publish.pkid = p.pkid;
    publish
}

fn properties(props: Option<wire::PublishProperties>) -> PublishProperties {
    let Some(props) = props else {
        return PublishProperties::default();
    };
    PublishProperties {
        content_type: props.content_type,
        correlation_data: props.correlation_data,
        response_topic: props.response_topic,
        message_expiry: props.message_expiry_interval,
        user: props.user_properties,
    }
}

fn from_wire(packet: wire::Packet) -> Packet {
    match packet {
        wire::Packet::Connect(c, _, login) => Packet::Connect(Connect {
            client_id: c.client_id,
            clean: c.clean_start,
            keep_alive: c.keep_alive,
            login: login.map(|l| (l.username, l.password)),
        }),
        wire::Packet::ConnAck(ack) => Packet::ConnAck {
            refused: (ack.code != ConnectReturnCode::Success).then(|| format!("{:?}", ack.code)),
            session_present: ack.session_present,
        },
        wire::Packet::Publish(p) => Packet::Publish(Publish {
            topic: String::from_utf8_lossy(&p.topic).into_owned(),
            qos: p.qos as u8,
            retain: p.retain,
            pkid: p.pkid,
            payload: p.payload,
            properties: properties(p.properties),
        }),
        wire::Packet::PubAck(a) => Packet::PubAck {
            pkid: a.pkid,
            refused: refused_ack(a.reason),
        },
        wire::Packet::PubRec(a) => Packet::PubRec {
            pkid: a.pkid,
            refused: refused_rec(a.reason),
        },
        other => from_wire_rest(other),
    }
}

fn from_wire_rest(packet: wire::Packet) -> Packet {
    match packet {
        wire::Packet::PubRel(a) => Packet::PubRel { pkid: a.pkid },
        wire::Packet::PubComp(a) => Packet::PubComp { pkid: a.pkid },
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
                .iter()
                .find(|c| !matches!(c, SubscribeReasonCode::Success(_)))
                .map(|c| format!("{c:?}")),
        },
        wire::Packet::PingReq(_) => Packet::PingReq,
        wire::Packet::PingResp(_) => Packet::PingResp,
        wire::Packet::Disconnect(d) => Packet::Disconnect {
            reason: (d.reason_code != DisconnectReasonCode::NormalDisconnection)
                .then(|| format!("{:?}", d.reason_code)),
        },
        _ => Packet::Other,
    }
}

fn refused_ack(reason: PubAckReason) -> Option<String> {
    match reason {
        PubAckReason::Success | PubAckReason::NoMatchingSubscribers => None,
        other => Some(format!("{other:?}")),
    }
}

fn refused_rec(reason: PubRecReason) -> Option<String> {
    match reason {
        PubRecReason::Success | PubRecReason::NoMatchingSubscribers => None,
        other => Some(format!("{other:?}")),
    }
}

/// The correlation data of `text`.
pub fn correlation(text: &str) -> Bytes {
    Bytes::copy_from_slice(text.as_bytes())
}
