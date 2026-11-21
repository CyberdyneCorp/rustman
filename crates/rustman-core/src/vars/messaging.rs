//! Resolving the messaging kinds (`kind: kafka`, `kind: mqtt`,
//! `kind: amqp`) before a client connects.
//!
//! `{{name}}` is interpolated in every string field: brokers, the
//! destination, offsets given as timestamps, credentials, TLS paths, the
//! Schema Registry settings, message keys, values (text, base64, file
//! paths and the string values of JSON) and headers. Every missing name
//! is reported at once, every secret used lands in `connection.secrets`,
//! so [`ResolvedMessaging::redactor`] covers the whole operation, and
//! payload files are checked to stay inside the workspace.

use std::fmt;

use serde_json::Value;

use super::graphql::string_values;
use super::request::{
    ResolvedBody, ResolvedFile, ResolvedRequest, auth, contain_files, settings, tls_files,
};
use super::resolver::{Interpolator, ResolveError, Resolver};
use crate::model::{
    AckMode, Acks, BrokerAuth, KafkaSecurity, Message, MessageProperties, MessagingKind,
    MessagingLimits, MessagingRequest, MqttVersion, Operation, Payload, Qos, Request,
    SchemaRegistry, Start, StartOffset,
};
use crate::paths::PathEscape;
use crate::redact::Redactor;
use crate::workspace::RequestRef;

/// A key or value with its strings interpolated. A file keeps its
/// contained path; it is read when the message is sent.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedPayload {
    Text(String),
    Json(Value),
    Base64(String),
    File(ResolvedFile),
}

/// One message to produce, interpolated.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMessage {
    pub key: Option<ResolvedPayload>,
    pub value: Option<ResolvedPayload>,
    /// Name/value pairs in file order.
    pub headers: Vec<(String, String)>,
}

/// The Schema Registry of a request: `http` is a `GET` of its base URL
/// with the registry's auth and TLS settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRegistry {
    pub http: ResolvedRequest,
    pub subject: Option<String>,
    /// The subject version to encode with (`latest` when not set).
    pub version: Option<String>,
}

/// The MQTT settings of a request, with their defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMqtt {
    pub version: MqttVersion,
    pub qos: Qos,
    pub retain: bool,
    /// A fixed client id; `None` generates `rustman-<8 hex>`.
    pub client_id: Option<String>,
    pub clean_session: bool,
}

/// The AMQP settings of a request, with their defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAmqp {
    pub mandatory: bool,
    /// `manual` whenever `requeue` is set.
    pub ack: AckMode,
    pub requeue: bool,
}

/// The AsyncAPI message a request validates payloads against; the
/// document path is contained in the workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedAsyncApi {
    pub document: ResolvedFile,
    pub message: String,
}

/// A messaging request with every placeholder replaced. `connection`
/// carries the broker address as its URL, the TLS settings and files, and
/// every secret of the request. The `Debug` output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedMessaging {
    pub kind: MessagingKind,
    pub operation: Operation,
    pub connection: ResolvedRequest,
    /// Kafka bootstrap brokers, one `host:port` each.
    pub brokers: Vec<String>,
    /// The MQTT or AMQP broker URL.
    pub broker: Option<String>,
    pub topic: Option<String>,
    pub exchange: Option<String>,
    pub routing_key: Option<String>,
    pub binding_key: Option<String>,
    pub queue: Option<String>,
    pub partition: Option<i32>,
    pub partitions: Option<Vec<i32>>,
    pub offset: StartOffset,
    pub acks: Acks,
    pub security: Option<KafkaSecurity>,
    pub auth: Option<BrokerAuth>,
    /// The request has a `tls` block.
    pub tls: bool,
    pub schema_registry: Option<ResolvedRegistry>,
    pub messages: Vec<ResolvedMessage>,
    pub limits: MessagingLimits,
    pub start: Start,
    pub mqtt: ResolvedMqtt,
    pub amqp: ResolvedAmqp,
    /// MQTT 5 and AMQP message properties, interpolated.
    pub properties: MessageProperties,
    pub asyncapi: Option<ResolvedAsyncApi>,
}

impl ResolvedMessaging {
    /// The redactor for everything shown or recorded about this operation.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(&self.connection)
    }

    /// The destination as messages name it: the topic, the queue, or
    /// `exchange/routing key`.
    pub fn destination(&self) -> String {
        if let Some(topic) = self.topic.as_ref().or(self.queue.as_ref()) {
            return topic.clone();
        }
        let exchange = self.exchange.as_deref().unwrap_or_default();
        let key = self.routing_key.as_ref().or(self.binding_key.as_ref());
        match key {
            Some(key) => format!("{exchange}/{key}"),
            None => exchange.to_owned(),
        }
    }
}

impl fmt::Debug for ResolvedMessaging {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedMessaging {{ kind: {:?}, operation: {:?}, connection: {:?}, brokers: {:?}, broker: {:?}, destination: {:?}, partition: {:?}, partitions: {:?}, offset: {:?}, acks: {:?}, security: {:?}, auth: {:?}, schema_registry: {:?}, messages: {:?}, limits: {:?}, start: {:?}, mqtt: {:?}, amqp: {:?}, properties: {:?}, asyncapi: {:?} }}",
            self.kind,
            self.operation,
            self.connection,
            self.brokers,
            self.broker,
            self.destination(),
            self.partition,
            self.partitions,
            self.offset,
            self.acks,
            self.security,
            self.auth,
            self.schema_registry,
            self.messages,
            self.limits,
            self.start,
            self.mqtt,
            self.amqp,
            self.properties,
            self.asyncapi,
        );
        f.write_str(&self.redactor().text(&raw))
    }
}

/// Resolve the messaging request `request` through `resolver`.
pub fn resolve_messaging(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedMessaging, ResolveError> {
    let Request::Messaging(messaging) = request.request() else {
        return Err(ResolveError::Unsupported(format!(
            "request kind '{}' is not a messaging request",
            request.request().kind()
        )));
    };
    let mut i = resolver.interpolator();
    let resolved = interpolate(&mut i, messaging);
    let (mut resolved, secrets) = i.finish(resolved)?;
    resolved.connection.secrets = secrets.clone();
    add_literal_credentials(&mut resolved);
    contain_files(request, &mut resolved.connection)?;
    if let Some(registry) = &mut resolved.schema_registry {
        registry.http.secrets = secrets;
        contain_files(request, &mut registry.http)?;
    }
    contain_payloads(request, &mut resolved)?;
    if let Some(asyncapi) = &mut resolved.asyncapi {
        asyncapi.document.path = request.contain_file(&asyncapi.document.raw)?;
    }
    Ok(resolved)
}

/// The name literal broker credentials are redacted under.
pub const LITERAL_CREDENTIAL: &str = "password";

/// Treat the credentials written as literals (`security.password`,
/// `auth.password`, the password of a broker URL) as secrets of the
/// connection, so every redactor built from it replaces them.
fn add_literal_credentials(resolved: &mut ResolvedMessaging) {
    for value in literal_credentials(resolved) {
        let known = resolved
            .connection
            .secrets
            .iter()
            .any(|s| s.expose() == &value);
        if !known {
            let secret = super::resolver::UsedSecret::new(LITERAL_CREDENTIAL, value);
            resolved.connection.secrets.push(secret);
        }
    }
}

/// Every non-empty password of `resolved`, as written and, for a URL,
/// percent-decoded.
fn literal_credentials(resolved: &ResolvedMessaging) -> Vec<String> {
    let security = resolved.security.as_ref().and_then(|s| s.password.clone());
    let auth = resolved.auth.as_ref().and_then(|a| a.password.clone());
    let mut found: Vec<String> = [security, auth].into_iter().flatten().collect();
    if let Some(password) = resolved.broker.as_deref().and_then(url_password) {
        found.push(password.to_owned());
        found.push(percent_decode(password));
    }
    found.retain(|v| !v.is_empty());
    found.dedup();
    found
}

/// The password of the userinfo of `url` (`scheme://user:password@host`).
fn url_password(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (userinfo, _) = authority.rsplit_once('@')?;
    userinfo.split_once(':').map(|(_, password)| password)
}

/// `text` with each `%XX` escape decoded; invalid escapes and invalid
/// UTF-8 are kept as written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn interpolate(i: &mut Interpolator<'_, '_>, m: &MessagingRequest) -> ResolvedMessaging {
    let mut text = |t: &Option<String>| t.as_deref().map(|t| i.text(t).text);
    let topic = text(&m.topic);
    let exchange = text(&m.exchange);
    let routing_key = text(&m.routing_key);
    let binding_key = text(&m.binding_key);
    let queue = text(&m.queue);
    let broker = text(&m.broker);
    let brokers = broker_list(i, m);
    let address = broker.clone().unwrap_or_else(|| brokers.join(","));
    ResolvedMessaging {
        kind: m.messaging_kind(),
        operation: m.effective_operation(),
        connection: connection(i, &address, m),
        brokers,
        broker,
        topic,
        exchange,
        routing_key,
        binding_key,
        queue,
        partition: m.partition,
        partitions: m.partitions.clone(),
        offset: offset(i, m.effective_offset()),
        acks: m.effective_acks(),
        security: m.security.as_deref().map(|s| security(i, s)),
        auth: m.auth.as_ref().map(|a| broker_auth(i, a)),
        tls: m.tls.is_some(),
        schema_registry: m.schema_registry.as_ref().map(|r| registry(i, r)),
        messages: m.message_list().iter().map(|msg| message(i, msg)).collect(),
        limits: m.effective_limits(),
        start: m.effective_start(),
        mqtt: mqtt(i, m),
        amqp: amqp(m),
        properties: properties(i, m.properties.as_ref()),
        asyncapi: m.asyncapi.as_ref().map(|a| ResolvedAsyncApi {
            document: ResolvedFile::unchecked(i.text(a.document.get()).text),
            message: i.text(&a.message).text,
        }),
    }
}

fn mqtt(i: &mut Interpolator<'_, '_>, m: &MessagingRequest) -> ResolvedMqtt {
    ResolvedMqtt {
        version: m.effective_protocol_version(),
        qos: m.effective_qos(),
        retain: m.retain.unwrap_or(false),
        client_id: m.client_id.as_deref().map(|c| i.text(c).text),
        clean_session: m.clean_session.unwrap_or(true),
    }
}

fn amqp(m: &MessagingRequest) -> ResolvedAmqp {
    let requeue = m.requeue.unwrap_or(false);
    let ack = if requeue {
        AckMode::Manual
    } else {
        m.ack.unwrap_or_default()
    };
    ResolvedAmqp {
        mandatory: m.mandatory.unwrap_or(false),
        ack,
        requeue,
    }
}

fn properties(i: &mut Interpolator<'_, '_>, p: Option<&MessageProperties>) -> MessageProperties {
    let Some(p) = p else {
        return MessageProperties::default();
    };
    let mut text = |t: &Option<String>| t.as_deref().map(|t| i.text(t).text);
    MessageProperties {
        content_type: text(&p.content_type),
        correlation_id: text(&p.correlation_id),
        reply_to: text(&p.reply_to),
        response_topic: text(&p.response_topic),
        ..p.clone()
    }
}

/// The Kafka brokers, each list entry split on commas.
fn broker_list(i: &mut Interpolator<'_, '_>, m: &MessagingRequest) -> Vec<String> {
    let Some(brokers) = &m.brokers else {
        return Vec::new();
    };
    let mut list = Vec::new();
    for entry in &brokers.0 {
        let text = i.text(entry).text;
        let parts = text.split(',').map(str::trim).filter(|b| !b.is_empty());
        list.extend(parts.map(str::to_owned));
    }
    list
}

/// The broker address with the request's TLS settings.
fn connection(
    i: &mut Interpolator<'_, '_>,
    address: &str,
    m: &MessagingRequest,
) -> ResolvedRequest {
    let settings = settings(i, m.http_settings().as_ref());
    let tls = tls_files(&settings);
    ResolvedRequest {
        method: m.effective_operation().as_str().to_ascii_uppercase(),
        url: super::resolver::Resolved::literal(address),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings,
        tls,
        secrets: Vec::new(),
    }
}

fn offset(i: &mut Interpolator<'_, '_>, offset: StartOffset) -> StartOffset {
    match offset {
        StartOffset::Timestamp(at) => StartOffset::Timestamp(i.text(&at).text),
        other => other,
    }
}

fn security(i: &mut Interpolator<'_, '_>, s: &KafkaSecurity) -> KafkaSecurity {
    KafkaSecurity {
        protocol: s.protocol,
        mechanism: s.mechanism,
        username: s.username.as_deref().map(|u| i.text(u).text),
        password: s.password.as_deref().map(|p| i.text(p).text),
    }
}

fn broker_auth(i: &mut Interpolator<'_, '_>, a: &BrokerAuth) -> BrokerAuth {
    BrokerAuth {
        username: i.text(&a.username).text,
        password: a.password.as_deref().map(|p| i.text(p).text),
    }
}

fn registry(i: &mut Interpolator<'_, '_>, r: &SchemaRegistry) -> ResolvedRegistry {
    let settings = settings(i, r.tls.as_ref().map(|t| t.settings()).as_ref());
    let tls = tls_files(&settings);
    let http = ResolvedRequest {
        method: "GET".to_owned(),
        url: i.text(&r.url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: r.auth.as_ref().map(|a| auth(i, a)),
        settings,
        tls,
        secrets: Vec::new(),
    };
    ResolvedRegistry {
        http,
        subject: r.subject.as_deref().map(|s| i.text(s).text),
        version: r
            .schema_version
            .as_ref()
            .map(|v| i.text(&v.path_segment()).text),
    }
}

fn message(i: &mut Interpolator<'_, '_>, m: &Message) -> ResolvedMessage {
    let headers = m.headers.as_deref().unwrap_or_default();
    ResolvedMessage {
        key: m.key.as_ref().map(|p| payload(i, p)),
        value: m.value.as_ref().map(|p| payload(i, p)),
        headers: headers
            .iter()
            .map(|h| (i.text(&h.name).text, i.text(&h.value).text))
            .collect(),
    }
}

fn payload(i: &mut Interpolator<'_, '_>, p: &Payload) -> ResolvedPayload {
    match p {
        Payload::Text(text) => ResolvedPayload::Text(i.text(text).text),
        Payload::Json(value) => ResolvedPayload::Json(string_values(i, value)),
        Payload::Base64(data) => ResolvedPayload::Base64(i.text(data).text),
        Payload::File(path) => {
            ResolvedPayload::File(ResolvedFile::unchecked(i.text(path.get()).text))
        }
    }
}

/// Check every payload file stays inside the workspace. The error shows
/// the resolved path with any secret redacted.
fn contain_payloads(
    request: &RequestRef<'_>,
    resolved: &mut ResolvedMessaging,
) -> Result<(), ResolveError> {
    let redactor = resolved.redactor();
    for message in &mut resolved.messages {
        for payload in [&mut message.key, &mut message.value].into_iter().flatten() {
            if let ResolvedPayload::File(file) = payload {
                file.path = request
                    .contain_file(&file.raw)
                    .map_err(|PathEscape(path)| PathEscape(redactor.text(&path).into_owned()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_passwords_are_found_and_decoded() {
        assert_eq!(url_password("mqtt://app:pw%40x@h:1883/"), Some("pw%40x"));
        assert_eq!(url_password("amqp://guest@rabbit"), None);
        assert_eq!(url_password("amqp://rabbit:5672/v"), None);
        assert_eq!(percent_decode("pw%40x%2"), "pw@x%2");
        assert_eq!(percent_decode("a%2Fb+c"), "a/b+c");
    }
}
