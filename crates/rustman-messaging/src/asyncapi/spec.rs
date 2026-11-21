//! Serde models of the AsyncAPI 2.6 and 3.0 subset Rustman reads
//! (servers, channels, operations, messages, payloads, examples and basic
//! bindings), normalized into one [`AsyncApi`] model (design D5).
//!
//! The document is bundled first, so every `$ref` left points into
//! `#/components` and is followed with the `openapi-import` resolver.
//! 3.0 operations name their channel and messages by reference; those
//! references are marked with `x-rustman-channel`/`x-rustman-message`
//! before bundling (see [`mark_references`]) so the names survive.

use std::collections::BTreeMap;

use rustman_core::model::{MessagingKind, Operation};
use rustman_explore::openapi::lower::deref;
use rustman_explore::openapi::pointer;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::AsyncApiError;

/// The extension that keeps a 3.0 operation's channel name.
pub const CHANNEL_MARK: &str = "x-rustman-channel";
/// The extension that keeps a 3.0 operation message's name.
pub const MESSAGE_MARK: &str = "x-rustman-message";
/// The extension that keeps the name of a 3.0 channel's server.
pub const SERVER_MARK: &str = "x-rustman-server";

/// The AsyncAPI lines Rustman reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V2_6,
    V3_0,
}

/// The version `doc` declares; anything but 2.6 and 3.0 is refused.
pub fn version(doc: &Value, source: &str) -> Result<Version, AsyncApiError> {
    let declared = match doc.get("asyncapi") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(AsyncApiError::NotAsyncApi(source.to_owned())),
    };
    let short: Vec<&str> = declared.split('.').take(2).collect();
    match short.as_slice() {
        ["2", "6"] => Ok(Version::V2_6),
        ["3", "0"] => Ok(Version::V3_0),
        _ => Err(AsyncApiError::UnsupportedVersion(short.join("."))),
    }
}

/// A server, with its variables' defaults substituted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub protocol: String,
    /// The messaging kind of the protocol, when Rustman speaks it.
    pub kind: Option<MessagingKind>,
    /// `host[:port]`.
    pub host: String,
    pub pathname: Option<String>,
    pub description: Option<String>,
}

/// A message with its payload schema and an example payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub name: String,
    /// The JSON pointer of the message object in the bundled document.
    pub pointer: String,
    pub payload: Option<Value>,
    /// The first documented example payload.
    pub example: Option<Value>,
    /// The Kafka binding's key schema.
    pub key: Option<Value>,
}

impl Message {
    /// The pointer of the payload schema, when there is one.
    pub fn payload_pointer(&self) -> Option<String> {
        self.payload
            .as_ref()
            .map(|_| format!("{}/payload", self.pointer))
    }
}

/// The basic bindings of an operation and its channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bindings {
    pub mqtt_qos: Option<u8>,
    pub mqtt_retain: Option<bool>,
    pub amqp_exchange: Option<String>,
    pub amqp_queue: Option<String>,
}

/// One operation: who sends what on which channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub id: String,
    pub operation: Operation,
    pub summary: Option<String>,
    /// The channel's key in the document.
    pub channel: String,
    /// The topic, routing key or queue the channel stands for.
    pub address: String,
    /// The servers the channel is on (empty for all).
    pub servers: Vec<String>,
    pub messages: Vec<Message>,
    pub bindings: Bindings,
}

/// A document normalized from either version.
#[derive(Clone, Debug, PartialEq)]
pub struct AsyncApi {
    pub version: Version,
    pub title: String,
    pub servers: Vec<Server>,
    pub operations: Vec<Op>,
    /// Every named message (components, channels, operations).
    pub messages: Vec<Message>,
    pub warnings: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Info {
    title: String,
}

#[derive(Deserialize)]
struct ServerVariable {
    default: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawServer {
    /// 2.6: `host[:port][/path]`, maybe with a scheme.
    url: Option<String>,
    /// 3.0.
    host: Option<String>,
    pathname: Option<String>,
    protocol: String,
    description: Option<String>,
    #[serde(default)]
    variables: BTreeMap<String, ServerVariable>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OperationV2 {
    operation_id: Option<String>,
    summary: Option<String>,
    message: Option<Value>,
    bindings: Option<Value>,
}

#[derive(Deserialize)]
struct ChannelV2 {
    publish: Option<OperationV2>,
    subscribe: Option<OperationV2>,
    bindings: Option<Value>,
    servers: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ChannelV3 {
    address: Option<String>,
    #[serde(default)]
    messages: BTreeMap<String, Value>,
    bindings: Option<Value>,
    servers: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct OperationV3 {
    action: String,
    channel: Value,
    summary: Option<String>,
    messages: Option<Vec<Value>>,
    bindings: Option<Value>,
}

/// Add the channel, message and server names to the references of 3.0
/// operations and channels, so they survive bundling.
pub fn mark_references(doc: &mut Value) {
    let channels = doc.get_mut("channels").and_then(Value::as_object_mut);
    for channel in channels.into_iter().flat_map(|c| c.values_mut()) {
        let servers = channel.get_mut("servers").and_then(Value::as_array_mut);
        for server in servers.into_iter().flatten() {
            mark(server, SERVER_MARK, "servers", 2);
        }
    }
    let Some(operations) = doc.get_mut("operations").and_then(Value::as_object_mut) else {
        return;
    };
    for operation in operations.values_mut() {
        if let Some(channel) = operation.get_mut("channel") {
            mark(channel, CHANNEL_MARK, "channels", 2);
        }
        let messages = operation.get_mut("messages").and_then(Value::as_array_mut);
        for message in messages.into_iter().flatten() {
            mark(message, MESSAGE_MARK, "channels", 4);
        }
    }
}

/// Copy token `index` of the `#/<section>/...` reference in `node` into
/// `mark`.
fn mark(node: &mut Value, mark: &str, section: &str, index: usize) {
    let Some(reference) = node.get("$ref").and_then(Value::as_str) else {
        return;
    };
    let tokens = reference
        .strip_prefix('#')
        .and_then(pointer::from_fragment)
        .map(|p| tokens(&p))
        .unwrap_or_default();
    if tokens.first().map(String::as_str) == Some(section)
        && let Some(name) = tokens.get(index - 1).cloned()
    {
        node[mark] = Value::String(name);
    }
}

/// The unescaped tokens of a JSON pointer.
fn tokens(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn typed<T: DeserializeOwned>(value: &Value, at: &str) -> Result<T, AsyncApiError> {
    serde_json::from_value(value.clone())
        .map_err(|e| AsyncApiError::Invalid(format!("{}: {e}", pointer::display(at))))
}

/// Normalize the bundled document `doc` of `version`.
pub fn normalize(doc: &Value, version: Version) -> Result<AsyncApi, AsyncApiError> {
    let info: Info = doc
        .get("info")
        .map(|i| typed(i, "/info"))
        .transpose()?
        .unwrap_or_default();
    let mut model = AsyncApi {
        version,
        title: info.title,
        servers: servers(doc)?,
        operations: Vec::new(),
        messages: Vec::new(),
        warnings: Vec::new(),
    };
    let mut reader = Reader {
        doc,
        warnings: Vec::new(),
    };
    model.operations = match version {
        Version::V2_6 => reader.operations_v2()?,
        Version::V3_0 => reader.operations_v3()?,
    };
    model.messages = reader.named_messages(&model.operations, version)?;
    model.warnings = reader.warnings;
    Ok(model)
}

fn servers(doc: &Value) -> Result<Vec<Server>, AsyncApiError> {
    let Some(map) = doc.get("servers").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for name in map.keys() {
        let at = pointer::push("/servers", name);
        let (target, value) = deref(doc, &at).map_err(AsyncApiError::Invalid)?;
        let raw: RawServer = typed(value, &target)?;
        out.push(server(name, raw));
    }
    Ok(out)
}

fn server(name: &str, raw: RawServer) -> Server {
    let mut host = raw.host.or(raw.url).unwrap_or_default();
    let mut pathname = raw.pathname;
    for (var, value) in &raw.variables {
        let default = value.default.clone().unwrap_or_default();
        host = host.replace(&format!("{{{var}}}"), &default);
        pathname = pathname.map(|p| p.replace(&format!("{{{var}}}"), &default));
    }
    if let Some((_, rest)) = host.split_once("://") {
        host = rest.to_owned();
    }
    if let Some((authority, path)) = host.clone().split_once('/') {
        host = authority.to_owned();
        pathname = pathname.or(Some(format!("/{path}")));
    }
    Server {
        name: name.to_owned(),
        kind: protocol_kind(&raw.protocol),
        protocol: raw.protocol,
        host,
        pathname,
        description: raw.description,
    }
}

/// The messaging kind of an AsyncAPI protocol name.
pub fn protocol_kind(protocol: &str) -> Option<MessagingKind> {
    match protocol.to_ascii_lowercase().as_str() {
        "kafka" | "kafka-secure" => Some(MessagingKind::Kafka),
        "mqtt" | "mqtts" | "secure-mqtt" | "mqtt5" => Some(MessagingKind::Mqtt),
        "amqp" | "amqps" => Some(MessagingKind::Amqp),
        _ => None,
    }
}

struct Reader<'a> {
    doc: &'a Value,
    warnings: Vec<String>,
}

impl Reader<'_> {
    fn operations_v2(&mut self) -> Result<Vec<Op>, AsyncApiError> {
        let mut out = Vec::new();
        let Some(channels) = self.doc.get("channels").and_then(Value::as_object) else {
            return Ok(out);
        };
        for (name, value) in channels {
            let at = pointer::push("/channels", name);
            let channel: ChannelV2 = typed(value, &at)?;
            let kinds = [
                (Operation::Produce, "publish", channel.publish.as_ref()),
                (Operation::Consume, "subscribe", channel.subscribe.as_ref()),
            ];
            for (operation, action, raw) in kinds {
                let Some(raw) = raw else { continue };
                let op_at = pointer::push(&at, action);
                out.push(self.op_v2(name, &channel, operation, action, raw, &op_at)?);
            }
        }
        Ok(out)
    }

    fn op_v2(
        &mut self,
        name: &str,
        channel: &ChannelV2,
        operation: Operation,
        action: &str,
        raw: &OperationV2,
        at: &str,
    ) -> Result<Op, AsyncApiError> {
        let id = raw
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{action}-{name}"));
        let messages = match &raw.message {
            Some(_) => self.messages_v2(&pointer::push(at, "message"), &format!("{id}-message"))?,
            None => Vec::new(),
        };
        let bindings = self.bindings(&[channel.bindings.as_ref(), raw.bindings.as_ref()], at);
        Ok(Op {
            id,
            operation,
            summary: raw.summary.clone(),
            channel: name.to_owned(),
            address: name.to_owned(),
            servers: channel.servers.clone().unwrap_or_default(),
            messages,
            bindings,
        })
    }

    /// The message (or `oneOf` messages) at `at`.
    fn messages_v2(&mut self, at: &str, fallback: &str) -> Result<Vec<Message>, AsyncApiError> {
        let (target, value) = deref(self.doc, at).map_err(AsyncApiError::Invalid)?;
        let Some(choices) = value.get("oneOf").and_then(Value::as_array) else {
            return Ok(vec![self.message(at, None, fallback)?]);
        };
        let one_of = pointer::push(&target, "oneOf");
        (0..choices.len())
            .map(|i| {
                let fallback = format!("{fallback}-{}", i + 1);
                self.message(&pointer::push(&one_of, &i.to_string()), None, &fallback)
            })
            .collect()
    }

    fn operations_v3(&mut self) -> Result<Vec<Op>, AsyncApiError> {
        let mut out = Vec::new();
        let Some(operations) = self.doc.get("operations").and_then(Value::as_object) else {
            return Ok(out);
        };
        for (id, value) in operations {
            let at = pointer::push("/operations", id);
            let raw: OperationV3 = typed(value, &at)?;
            out.push(self.op_v3(id, &raw, &at)?);
        }
        Ok(out)
    }

    fn op_v3(&mut self, id: &str, raw: &OperationV3, at: &str) -> Result<Op, AsyncApiError> {
        let operation = match raw.action.as_str() {
            "send" => Operation::Produce,
            "receive" => Operation::Consume,
            other => {
                return Err(AsyncApiError::Invalid(format!(
                    "{at}: action must be send or receive, not '{other}'"
                )));
            }
        };
        let name = raw
            .channel
            .get(CHANNEL_MARK)
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned();
        let channel_at = pointer::push(at, "channel");
        let (target, value) = deref(self.doc, &channel_at).map_err(AsyncApiError::Invalid)?;
        let channel: ChannelV3 = typed(value, &target)?;
        let messages = self.messages_v3(at, raw, &channel, &target)?;
        let bindings = self.bindings(&[channel.bindings.as_ref(), raw.bindings.as_ref()], at);
        Ok(Op {
            id: id.to_owned(),
            operation,
            summary: raw.summary.clone(),
            address: channel.address.clone().unwrap_or_else(|| name.clone()),
            channel: name,
            servers: channel
                .servers
                .iter()
                .flatten()
                .filter_map(server_name)
                .collect(),
            messages,
            bindings,
        })
    }

    /// The operation's messages, or every message of its channel.
    fn messages_v3(
        &mut self,
        at: &str,
        raw: &OperationV3,
        channel: &ChannelV3,
        channel_at: &str,
    ) -> Result<Vec<Message>, AsyncApiError> {
        match &raw.messages {
            Some(list) if !list.is_empty() => {
                let base = pointer::push(at, "messages");
                list.iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let name = m.get(MESSAGE_MARK).and_then(Value::as_str);
                        self.message(&pointer::push(&base, &i.to_string()), name, "message")
                    })
                    .collect()
            }
            _ => {
                let base = pointer::push(channel_at, "messages");
                channel
                    .messages
                    .keys()
                    .map(|key| self.message(&pointer::push(&base, key), Some(key), key))
                    .collect()
            }
        }
    }

    /// The message at `at`: named by its `name`, else `declared`, else
    /// the key it references, its `messageId`, or `fallback`.
    fn message(
        &mut self,
        at: &str,
        declared: Option<&str>,
        fallback: &str,
    ) -> Result<Message, AsyncApiError> {
        let reference = self
            .doc
            .pointer(at)
            .and_then(|v| v.get("$ref"))
            .and_then(Value::as_str);
        let referenced = reference
            .and_then(|r| r.rsplit('/').next())
            .map(str::to_owned);
        let (target, value) = deref(self.doc, at).map_err(AsyncApiError::Invalid)?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| declared.map(str::to_owned))
            .or(referenced)
            .or_else(|| {
                value
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| fallback.to_owned());
        Ok(Message {
            name,
            payload: value.get("payload").cloned(),
            example: example(value),
            key: value.pointer("/bindings/kafka/key").cloned(),
            pointer: target,
        })
    }

    /// The basic bindings in `sources`; other bindings are ignored with a
    /// warning.
    fn bindings(&mut self, sources: &[Option<&Value>], at: &str) -> Bindings {
        let mut out = Bindings::default();
        for source in sources.iter().flatten() {
            let Some(map) = source.as_object() else {
                continue;
            };
            for (protocol, binding) in map {
                if !read_binding(&mut out, protocol, binding) {
                    self.warn_binding(protocol, at);
                }
            }
        }
        out
    }

    fn warn_binding(&mut self, protocol: &str, at: &str) {
        let warning = format!(
            "asyncapi: the {protocol} binding at {} is ignored",
            pointer::display(at)
        );
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    /// Every message by name: components first, then those of channels
    /// and operations.
    fn named_messages(
        &mut self,
        operations: &[Op],
        version: Version,
    ) -> Result<Vec<Message>, AsyncApiError> {
        let mut out = Vec::new();
        if let Some(map) = self
            .doc
            .pointer("/components/messages")
            .and_then(Value::as_object)
        {
            for key in map.keys() {
                let at = pointer::push("/components/messages", key);
                out.push(self.message(&at, Some(key), key)?);
            }
        }
        if version == Version::V3_0 {
            out.extend(self.channel_messages()?);
        }
        for op in operations {
            out.extend(op.messages.iter().cloned());
        }
        Ok(out)
    }

    fn channel_messages(&mut self) -> Result<Vec<Message>, AsyncApiError> {
        let mut out = Vec::new();
        let Some(channels) = self.doc.get("channels").and_then(Value::as_object) else {
            return Ok(out);
        };
        for (name, channel) in channels {
            let Some(messages) = channel.get("messages").and_then(Value::as_object) else {
                continue;
            };
            let base = pointer::push(&pointer::push("/channels", name), "messages");
            for key in messages.keys() {
                out.push(self.message(&pointer::push(&base, key), Some(key), key)?);
            }
        }
        Ok(out)
    }
}

/// Read one protocol's binding into `out`; `false` when it is not one
/// Rustman uses.
fn read_binding(out: &mut Bindings, protocol: &str, binding: &Value) -> bool {
    match protocol {
        "mqtt" => {
            out.mqtt_qos = out.mqtt_qos.or(binding
                .get("qos")
                .and_then(Value::as_u64)
                .and_then(|q| u8::try_from(q).ok()));
            out.mqtt_retain = out
                .mqtt_retain
                .or(binding.get("retain").and_then(Value::as_bool));
            true
        }
        "amqp" => {
            let name = |key: &str| {
                binding
                    .pointer(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            out.amqp_exchange = out.amqp_exchange.take().or(name("/exchange/name"));
            out.amqp_queue = out.amqp_queue.take().or(name("/queue/name"));
            true
        }
        "kafka" => true,
        _ => false,
    }
}

/// A 3.0 server reference's name.
fn server_name(reference: &Value) -> Option<String> {
    if let Some(name) = reference.get(SERVER_MARK).and_then(Value::as_str) {
        return Some(name.to_owned());
    }
    let reference = reference.get("$ref")?.as_str()?;
    reference
        .strip_prefix("#/servers/")
        .map(|n| n.replace("~1", "/").replace("~0", "~"))
}

/// The first example payload of a message.
fn example(message: &Value) -> Option<Value> {
    let examples = message.get("examples").and_then(Value::as_array)?;
    examples.iter().find_map(|e| e.get("payload").cloned())
}

/// The `components` of `doc`, for the synthesizer.
pub fn components(doc: &Value) -> Value {
    doc.get("components")
        .cloned()
        .unwrap_or(Value::Object(Map::new()))
}
