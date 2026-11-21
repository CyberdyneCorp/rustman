//! Broker endpoints (messaging-requests "Safety gating"): the one parse
//! of a broker address. The safety policy checks, and the connectors
//! dial, the host and port returned here, so the two can never read the
//! same text differently.
//!
//! A broker containing a backslash, a control character or whitespace is
//! refused outright, since URL parsers disagree on them. Hosts are
//! lowercased, lose a trailing dot and must be an IP literal or a DNS
//! name (`a-z`, `0-9`, `-`, `_`, `.`) whose last label is not numeric.
//! Errors never echo the broker, which may hold a password.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use lapin::uri::AMQPUri;
use rustman_core::model::MessagingKind;
use rustman_core::vars::ResolvedMessaging;
use url::Url;

use crate::MessagingError;

/// What an audit line shows for a broker that does not parse.
pub const INVALID_BROKER: &str = "«invalid broker»";

/// The port a Kafka broker without one is reached on.
pub const KAFKA_DEFAULT_PORT: u16 = 9092;

/// How the client reaches the broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Tls,
    Ws,
    Wss,
}

impl Transport {
    pub fn uses_tls(self) -> bool {
        matches!(self, Self::Tls | Self::Wss)
    }

    fn http_scheme(self) -> &'static str {
        if self.uses_tls() { "https" } else { "http" }
    }
}

/// A parsed broker address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub transport: Transport,
    /// Lowercase, without IPv6 brackets or a trailing dot.
    pub host: String,
    pub port: u16,
    /// `ws://`/`wss://` only: the URL the WebSocket handshake asks for,
    /// on `host`, without credentials.
    pub ws_url: Option<String>,
}

impl Endpoint {
    /// `host:port`, an IPv6 host in brackets: the address dialed.
    pub fn address(&self) -> String {
        format!("{}:{}", bracketed(&self.host), self.port)
    }

    /// The `http` or `https` URL an allowlist checks this broker as:
    /// `https` for TLS transports, always with the port.
    pub fn policy_url(&self) -> String {
        format!("{}://{}", self.transport.http_scheme(), self.address())
    }
}

fn bracketed(host: &str) -> String {
    match host.parse::<Ipv6Addr>() {
        Ok(_) => format!("[{host}]"),
        Err(_) => host.to_owned(),
    }
}

/// The endpoints of `resolved`: each Kafka bootstrap broker, or the MQTT
/// or AMQP broker. Never empty.
pub fn of(resolved: &ResolvedMessaging) -> Result<Vec<Endpoint>, MessagingError> {
    let endpoints = match resolved.kind {
        MessagingKind::Kafka => resolved
            .brokers
            .iter()
            .map(|b| kafka(b))
            .collect::<Result<Vec<_>, _>>()?,
        MessagingKind::Mqtt => vec![mqtt(broker_of(resolved))?],
        MessagingKind::Amqp => vec![amqp(broker_of(resolved))?.0],
    };
    if endpoints.is_empty() {
        return Err(MessagingError::Invalid(format!(
            "{}: no broker",
            resolved.kind.as_str()
        )));
    }
    Ok(endpoints)
}

fn broker_of(resolved: &ResolvedMessaging) -> &str {
    resolved.broker.as_deref().unwrap_or_default()
}

/// A Kafka bootstrap broker: `host[:port]` (port 9092 by default), with
/// no scheme, credentials or path. The policy reads it as `http://`
/// whatever the security protocol.
pub fn kafka(broker: &str) -> Result<Endpoint, MessagingError> {
    let invalid = invalid("kafka", broker);
    check_text(broker).map_err(&invalid)?;
    let url = Url::parse(&format!("kafka://{broker}")).map_err(|e| invalid(&e.to_string()))?;
    let bare = url.username().is_empty()
        && url.password().is_none()
        && url.path().is_empty()
        && url.query().is_none()
        && url.fragment().is_none();
    if !bare {
        return Err(invalid("expected host:port"));
    }
    Ok(Endpoint {
        transport: Transport::Tcp,
        host: url_host(&url).map_err(&invalid)?,
        port: url.port().unwrap_or(KAFKA_DEFAULT_PORT),
        ws_url: None,
    })
}

/// An MQTT broker: a `mqtt://`/`tcp://` (1883), `mqtts://`/`ssl://`
/// (8883), `ws://` (80) or `wss://` (443) URL, or `host[:port]` for
/// `mqtt://`.
pub fn mqtt(broker: &str) -> Result<Endpoint, MessagingError> {
    let invalid = invalid("mqtt", broker);
    check_text(broker).map_err(&invalid)?;
    let text = if broker.contains("://") {
        broker.to_owned()
    } else {
        format!("mqtt://{broker}")
    };
    let mut url = Url::parse(&text).map_err(|e| invalid(&e.to_string()))?;
    let (transport, default_port) = match url.scheme() {
        "mqtt" | "tcp" => (Transport::Tcp, 1883),
        "mqtts" | "ssl" => (Transport::Tls, 8883),
        "ws" => (Transport::Ws, 80),
        "wss" => (Transport::Wss, 443),
        other => return Err(invalid(&format!("unsupported scheme '{other}'"))),
    };
    let host = url_host(&url).map_err(&invalid)?;
    let port = url.port().unwrap_or(default_port);
    let ws_url = match transport {
        Transport::Ws | Transport::Wss => Some(websocket_url(&mut url, &host)),
        Transport::Tcp | Transport::Tls => None,
    };
    Ok(Endpoint {
        transport,
        host,
        port,
        ws_url,
    })
}

/// `url` on `host`, without credentials.
fn websocket_url(url: &mut Url, host: &str) -> String {
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let _ = url.set_host(Some(&bracketed(host)));
    url.to_string()
}

/// An AMQP broker URL, and the `lapin` URI dialed, whose host is the
/// endpoint's.
pub fn amqp(broker: &str) -> Result<(Endpoint, AMQPUri), MessagingError> {
    let invalid = invalid("amqp", broker);
    check_text(broker).map_err(&invalid)?;
    let mut uri = AMQPUri::from_str(broker).map_err(|e| invalid(&e))?;
    let host = normal_host(&uri.authority.host).map_err(&invalid)?;
    uri.authority.host.clone_from(&host);
    let transport = match uri.scheme {
        lapin::uri::AMQPScheme::AMQP => Transport::Tcp,
        lapin::uri::AMQPScheme::AMQPS => Transport::Tls,
    };
    let endpoint = Endpoint {
        transport,
        host,
        port: uri.authority.port,
        ws_url: None,
    };
    Ok((endpoint, uri))
}

/// The error for `broker` of `protocol`. It never echoes the broker,
/// whose text may hold a password no parser found: only its scheme and
/// [`INVALID_BROKER`].
fn invalid(protocol: &'static str, broker: &str) -> impl Fn(&str) -> MessagingError {
    let shown = shown_invalid(broker);
    move |why| MessagingError::Invalid(format!("{protocol}: invalid broker '{shown}': {why}"))
}

/// `<scheme>://«invalid broker»`, or the marker alone without a valid
/// scheme.
fn shown_invalid(broker: &str) -> String {
    let scheme = broker.split_once("://").map(|(scheme, _)| scheme);
    let valid = scheme.filter(|s| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    match valid {
        Some(scheme) => format!("{scheme}://{INVALID_BROKER}"),
        None => INVALID_BROKER.to_owned(),
    }
}

/// `broker` for display: any userinfo replaced by a marker (see
/// [`rustman_core::vars::broker_credentials`]).
pub fn without_userinfo(broker: &str) -> String {
    rustman_core::vars::broker_credentials::without_userinfo(broker)
}

/// Refuse text URL parsers disagree on: backslashes, control characters
/// and whitespace.
fn check_text(broker: &str) -> Result<(), &'static str> {
    if broker.is_empty() {
        return Err("empty");
    }
    if broker.contains('\\') {
        return Err("a backslash is not allowed");
    }
    if broker.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("control characters and whitespace are not allowed");
    }
    Ok(())
}

fn url_host(url: &Url) -> Result<String, &'static str> {
    let host = url.host_str().ok_or("no host")?;
    let host = host.strip_prefix('[').unwrap_or(host);
    normal_host(host.strip_suffix(']').unwrap_or(host))
}

/// `host` as an IP literal or a lowercase DNS name without a trailing
/// dot. A host whose last label is numeric must be a strict
/// dotted-decimal IPv4 address: URL parsers (and so the policy) read
/// `0177.0.0.1`, `127.1` or `0x7f.1` as IPv4 in octal, short or hex
/// form while a resolver may read them otherwise, so the text dialed and
/// the text checked could differ.
fn normal_host(host: &str) -> Result<String, &'static str> {
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return Ok(ip.to_string());
    }
    let name = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if let Ok(ip) = name.parse::<Ipv4Addr>() {
        return Ok(ip.to_string());
    }
    if ends_in_number(&name) {
        return Err("a numeric host must be a dotted-decimal IPv4 address");
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if name.is_empty() || name.len() > 253 || !name.split('.').all(label_ok) {
        return Err("the host must be an IP address or a DNS name");
    }
    Ok(name)
}

/// Whether the last label of `name` (lowercase, without a trailing dot)
/// is a number as the WHATWG URL host parser reads one: decimal or octal
/// digits, or `0x` with hex digits (or none). Such a host is parsed as
/// IPv4 by URL parsers.
fn ends_in_number(name: &str) -> bool {
    let mut labels = name.rsplit('.');
    let last = match labels.next() {
        Some("") => labels.next().unwrap_or_default(),
        Some(last) => last,
        None => "",
    };
    if last.is_empty() {
        return false;
    }
    if last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests;
