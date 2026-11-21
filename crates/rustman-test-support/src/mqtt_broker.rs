//! An in-process MQTT broker for hermetic tests: MQTT 3.1.1 and 5 over
//! TCP, TLS (optionally requiring a client certificate) or WebSocket.
//! It routes publishes to matching subscriptions (`+` and `#`), runs the
//! QoS 1 and 2 flows, checks a login when one is set, and logs every
//! packet it handles so tests can assert on the protocol exchange.
//! Behind the `mqtt` feature, since it speaks through `rustman-messaging`'s
//! MQTT codec and link.

use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use rumqttc::mqttbytes::matches;
use rustman_core::model::MqttVersion;
use rustman_messaging::mqtt::{Codec, Connect, Link, Packet, Publish};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::certs::TestCerts;
use crate::server::{Tls, server_config};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Tcp,
    /// TLS that requires a client certificate signed by the test CA.
    MutualTls,
    Ws,
}

#[derive(Default)]
struct State {
    log: Vec<String>,
    subscriptions: Vec<(String, u8, mpsc::UnboundedSender<Publish>)>,
    login: Option<(String, String)>,
}

type Shared = Arc<Mutex<State>>;

pub struct MqttBroker {
    port: u16,
    mode: Mode,
    state: Shared,
}

impl MqttBroker {
    pub async fn start(mode: Mode, certs: Option<&TestCerts>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Shared::default();
        let acceptor =
            certs.map(|c| TlsAcceptor::from(Arc::new(server_config(c, Tls::ClientAuth))));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(accept(tcp, mode, acceptor.clone(), shared.clone()));
            }
        });
        Self { port, mode, state }
    }

    /// Require this username and password.
    pub fn require_login(&self, user: &str, password: &str) {
        self.state.lock().unwrap().login = Some((user.to_owned(), password.to_owned()));
    }

    pub fn url(&self) -> String {
        match self.mode {
            Mode::Tcp => format!("mqtt://127.0.0.1:{}", self.port),
            Mode::MutualTls => format!("mqtts://localhost:{}", self.port),
            Mode::Ws => format!("ws://127.0.0.1:{}/mqtt", self.port),
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }
}

async fn accept(tcp: TcpStream, mode: Mode, acceptor: Option<TlsAcceptor>, state: Shared) {
    let link = match (mode, acceptor) {
        (Mode::MutualTls, Some(acceptor)) => match acceptor.accept(tcp).await {
            Ok(tls) => Link::Stream(Box::new(tls)),
            Err(e) => return note(&state, format!("TLS refused: {e}")),
        },
        (Mode::Ws, _) => {
            // The handshake callback's signature is tungstenite's.
            #[allow(clippy::result_large_err)]
            let callback = |_: &Request, mut response: Response| {
                response
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
                Ok(response)
            };
            let boxed: Box<dyn rustman_messaging::mqtt::Io> = Box::new(tcp);
            match tokio_tungstenite::accept_hdr_async(boxed, callback).await {
                Ok(ws) => Link::Ws(Box::new(ws)),
                Err(_) => return,
            }
        }
        _ => Link::Stream(Box::new(tcp)),
    };
    if let Some(connection) = Connection::open(link, state).await {
        connection.serve().await;
    }
}

fn note(state: &Shared, line: String) {
    state.lock().unwrap().log.push(line);
}

struct Connection {
    link: Link,
    codec: Codec,
    buf: BytesMut,
    state: Shared,
    deliveries: mpsc::UnboundedReceiver<Publish>,
    sender: mpsc::UnboundedSender<Publish>,
    next_pkid: u16,
}

impl Connection {
    /// Read the CONNECT (detecting the protocol version), check the
    /// login, and answer with a CONNACK.
    async fn open(mut link: Link, state: Shared) -> Option<Self> {
        let mut buf = BytesMut::new();
        let (codec, connect) = loop {
            if let Some(found) = detect(&buf) {
                break found;
            }
            if !link.recv(&mut buf).await.ok()? {
                return None;
            }
        };
        let _ = codec.read(&mut buf);
        let (sender, deliveries) = mpsc::unbounded_channel();
        let mut connection = Self {
            link,
            codec,
            buf,
            state,
            deliveries,
            sender,
            next_pkid: 0,
        };
        let refused = connection.refusal(&connect);
        connection.log(format!(
            "CONNECT {} {} clean={}",
            codec.0.as_str(),
            connect.client_id,
            connect.clean
        ));
        let ack = Packet::ConnAck {
            refused: refused.clone(),
            session_present: false,
        };
        connection.send(&ack).await;
        refused.is_none().then_some(connection)
    }

    fn refusal(&self, connect: &Connect) -> Option<String> {
        let wanted = self.state.lock().unwrap().login.clone();
        match wanted {
            Some(login) if connect.login.as_ref() != Some(&login) => {
                Some("BadUserNamePassword".to_owned())
            }
            _ => None,
        }
    }

    fn log(&self, line: String) {
        note(&self.state, line);
    }

    async fn send(&mut self, packet: &Packet) {
        let mut bytes = BytesMut::new();
        self.codec.write(packet, &mut bytes).unwrap();
        let _ = self.link.send(bytes).await;
    }

    async fn serve(mut self) {
        loop {
            let packet = match self.codec.read(&mut self.buf) {
                Ok(Some(packet)) => Some(packet),
                Ok(None) => None,
                Err(_) => return,
            };
            if let Some(packet) = packet {
                if !self.handle(packet).await {
                    return;
                }
                continue;
            }
            tokio::select! {
                received = self.link.recv(&mut self.buf) => {
                    if !received.unwrap_or(false) {
                        return;
                    }
                }
                Some(publish) = self.deliveries.recv() => self.deliver(publish).await,
            }
        }
    }

    async fn handle(&mut self, packet: Packet) -> bool {
        match packet {
            Packet::Publish(publish) => self.publish(publish).await,
            Packet::PubRel { pkid } => {
                self.log(format!("PUBREL {pkid}"));
                self.send(&Packet::PubComp { pkid }).await;
            }
            Packet::PubRec { pkid, .. } => {
                self.log(format!("PUBREC {pkid}"));
                self.send(&Packet::PubRel { pkid }).await;
            }
            Packet::PubAck { pkid, .. } => self.log(format!("PUBACK {pkid}")),
            Packet::PubComp { pkid } => self.log(format!("PUBCOMP {pkid}")),
            Packet::Subscribe { pkid, filter, qos } => {
                self.log(format!("SUBSCRIBE {filter} qos{qos}"));
                let sender = self.sender.clone();
                self.state
                    .lock()
                    .unwrap()
                    .subscriptions
                    .push((filter, qos, sender));
                self.send(&Packet::SubAck {
                    pkid,
                    refused: None,
                })
                .await;
            }
            Packet::PingReq => self.send(&Packet::PingResp).await,
            Packet::Disconnect { .. } => {
                self.log("DISCONNECT".to_owned());
                return false;
            }
            _ => {}
        }
        true
    }

    async fn publish(&mut self, publish: Publish) {
        self.log(format!(
            "PUBLISH {} qos{} retain={} {}",
            publish.topic,
            publish.qos,
            publish.retain,
            String::from_utf8_lossy(&publish.payload)
        ));
        match publish.qos {
            1 => {
                self.send(&Packet::PubAck {
                    pkid: publish.pkid,
                    refused: None,
                })
                .await
            }
            2 => {
                self.send(&Packet::PubRec {
                    pkid: publish.pkid,
                    refused: None,
                })
                .await
            }
            _ => {}
        }
        let state = self.state.lock().unwrap();
        for (filter, qos, sender) in &state.subscriptions {
            if matches(&publish.topic, filter) {
                let mut copy = publish.clone();
                copy.qos = copy.qos.min(*qos);
                let _ = sender.send(copy);
            }
        }
    }

    async fn deliver(&mut self, mut publish: Publish) {
        self.next_pkid += 1;
        publish.pkid = if publish.qos == 0 { 0 } else { self.next_pkid };
        publish.retain = false;
        self.send(&Packet::Publish(publish)).await;
    }
}

/// The codec and CONNECT at the start of `buf`, if one is complete. The
/// version is the protocol level after the `MQTT` protocol name (the v4
/// reader does not check it).
fn detect(buf: &BytesMut) -> Option<(Codec, Connect)> {
    let head = &buf[..buf.len().min(16)];
    let at = head.windows(6).position(|w| w == b"\x00\x04MQTT")?;
    let version = match head.get(at + 6)? {
        5 => MqttVersion::V5,
        _ => MqttVersion::V311,
    };
    let mut probe = buf.clone();
    match Codec(version).read(&mut probe) {
        Ok(Some(Packet::Connect(connect))) => Some((Codec(version), connect)),
        _ => None,
    }
}
