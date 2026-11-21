//! `rustman import asyncapi` (messaging-requests "AsyncAPI import"):
//! servers become environments and each operation a request file under a
//! folder per channel, `produce` for publish/send and `consume` for
//! subscribe/receive, with an example payload from `examples` or
//! synthesized from the payload schema. The bundled document is written
//! beside the requests as `asyncapi.json`, and requests whose message has
//! a payload schema reference it, so payloads are validated.
//!
//! The document is untrusted: every literal it supplies to a templated
//! field (payloads, keys, topics, exchanges, queues) has `{{` escaped, so
//! it cannot plant variable or `{{secret:...}}` references that would be
//! resolved at send time.

use std::fs;
use std::path::Path;

use rustman_core::model::{MessagingKind, Operation};
use rustman_core::vars::template::escape;
use rustman_explore::collection::names::kebab;
use rustman_explore::synth::Synthesizer;
use serde_json::{Value, json};

use super::spec::{self, Message, Op, Server};
use super::{AsyncApiError, Document};

/// The bundled document's file name in the collection folder.
pub const DOCUMENT_FILE: &str = "asyncapi.json";

/// A file to write, relative to its base folder with `/` separators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub text: String,
}

/// Everything an import writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPlan {
    /// The default collection folder: the title in kebab-case.
    pub collection: String,
    /// Environment files, relative to the workspace root.
    pub environments: Vec<GeneratedFile>,
    /// The document and request files, relative to the collection.
    pub files: Vec<GeneratedFile>,
    pub warnings: Vec<String>,
}

impl ImportPlan {
    /// The number of request files.
    pub fn requests(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.path.ends_with(".rman.yaml"))
            .count()
    }
}

/// What writing a plan did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    pub created: Vec<String>,
    /// Files that already existed and were left unchanged.
    pub kept: Vec<String>,
}

/// The files `document` imports to.
pub fn plan(document: &Document) -> ImportPlan {
    let model = &document.model;
    let mut warnings = model.warnings.clone();
    let environments = model
        .servers
        .iter()
        .filter_map(|s| environment(s, &mut warnings))
        .collect();
    let mut files = vec![GeneratedFile {
        path: DOCUMENT_FILE.to_owned(),
        text: serde_json::to_string_pretty(&document.bundled).unwrap_or_default() + "\n",
    }];
    let components = spec::components(&document.bundled);
    let synthesizer = Synthesizer::new(&components);
    let mut taken = Vec::new();
    for op in &model.operations {
        let Some(server) = server_for(op, &model.servers) else {
            warnings.push(format!(
                "asyncapi: operation '{}' has no Kafka, MQTT or AMQP server and was skipped",
                op.id
            ));
            continue;
        };
        let path = unique(
            format!("{}/{}", name(&op.channel), name(&op.id)),
            &mut taken,
        );
        let request = RequestText {
            op,
            kind: server.kind.unwrap_or(MessagingKind::Kafka),
            synthesizer: &synthesizer,
        };
        files.push(GeneratedFile {
            path: format!("{path}.rman.yaml"),
            text: request.render(),
        });
    }
    ImportPlan {
        collection: name(if model.title.is_empty() {
            "asyncapi"
        } else {
            &model.title
        }),
        environments,
        files,
        warnings,
    }
}

/// Write `plan`: environments under `root`, the rest under `dir`.
/// Existing files are kept.
pub fn write(plan: &ImportPlan, root: &Path, dir: &Path) -> Result<WriteOutcome, AsyncApiError> {
    let mut outcome = WriteOutcome::default();
    for file in &plan.environments {
        write_new(root, file, &mut outcome)?;
    }
    for file in &plan.files {
        write_new(dir, file, &mut outcome)?;
    }
    Ok(outcome)
}

fn write_new(
    base: &Path,
    file: &GeneratedFile,
    outcome: &mut WriteOutcome,
) -> Result<(), AsyncApiError> {
    let path = base.join(&file.path);
    let shown = path.display().to_string();
    if path.exists() {
        outcome.kept.push(shown);
        return Ok(());
    }
    let failed = |e: std::io::Error| AsyncApiError::Write {
        path: shown.clone(),
        message: e.to_string(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    fs::write(&path, &file.text).map_err(failed)?;
    outcome.created.push(shown);
    Ok(())
}

/// A folder or file name: kebab-case, never empty.
fn name(text: &str) -> String {
    let name = kebab(text);
    if name.is_empty() {
        "unnamed".to_owned()
    } else {
        name
    }
}

fn unique(base: String, taken: &mut Vec<String>) -> String {
    let mut candidate = base.clone();
    let mut n = 1;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    taken.push(candidate.clone());
    candidate
}

/// The server an operation runs against: the first of its channel's
/// servers that Rustman speaks, else the first such server overall.
fn server_for<'a>(op: &Op, servers: &'a [Server]) -> Option<&'a Server> {
    let usable = |s: &&Server| s.kind.is_some();
    let listed = servers
        .iter()
        .filter(usable)
        .find(|s| op.servers.contains(&s.name));
    listed.or_else(|| {
        servers
            .iter()
            .find(usable)
            .filter(|_| op.servers.is_empty())
    })
}

/// The environment of `server`, or a warning for a protocol Rustman does
/// not speak.
fn environment(server: &Server, warnings: &mut Vec<String>) -> Option<GeneratedFile> {
    let Some(kind) = server.kind else {
        warnings.push(format!(
            "asyncapi: server '{}' uses protocol '{}', which Rustman does not speak; no environment was created",
            server.name, server.protocol
        ));
        return None;
    };
    let (variable, value) = match kind {
        MessagingKind::Kafka => ("brokers", server.host.clone()),
        MessagingKind::Mqtt | MessagingKind::Amqp => ("broker", broker_url(server)),
    };
    let comment = match &server.description {
        Some(d) => format!("# {}\n", d.lines().next().unwrap_or_default()),
        None => String::new(),
    };
    Some(GeneratedFile {
        path: format!("environments/{}.yaml", name(&server.name)),
        text: format!("{comment}variables:\n  {variable}: {}\n", quoted(&value)),
    })
}

/// The `mqtt[s]://` or `amqp[s]://` URL of a server.
fn broker_url(server: &Server) -> String {
    let scheme = match server.protocol.to_ascii_lowercase().as_str() {
        "mqtts" | "secure-mqtt" => "mqtts",
        "amqps" => "amqps",
        "amqp" => "amqp",
        _ => "mqtt",
    };
    let path = server.pathname.as_deref().unwrap_or_default();
    format!("{scheme}://{}{path}", server.host)
}

/// A YAML scalar: JSON quoting is valid YAML.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// `value` on one line, as JSON (valid YAML flow style).
fn inline(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `value` with every string value escaped. Object keys are left as they
/// are: messaging payloads interpolate string values only.
fn escape_values(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(escape(text)),
        Value::Array(items) => Value::Array(items.iter().map(escape_values).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), escape_values(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Channel parameters (`{id}`) as variables, or as `+` in an MQTT
/// subscription; the parameter names. Everything else is an escaped
/// literal, so `{{...}}` in an address stays text.
fn address(op: &Op, kind: MessagingKind) -> (String, Vec<String>) {
    let wildcard = kind == MessagingKind::Mqtt && op.operation == Operation::Consume;
    let mut out = String::new();
    let mut literal = String::new();
    let mut params = Vec::new();
    let mut rest = op.address.as_str();
    while let Some((before, after)) = rest.split_once('{') {
        literal.push_str(before);
        let param = after.split_once('}').filter(|(name, tail)| {
            is_param(name) && !literal.ends_with('{') && !tail.starts_with('}')
        });
        let Some((name, tail)) = param else {
            literal.push('{');
            rest = after;
            continue;
        };
        out.push_str(&escape(&std::mem::take(&mut literal)));
        if wildcard {
            out.push('+');
        } else {
            out.push_str(&format!("{{{{{name}}}}}"));
            params.push(name.to_owned());
        }
        rest = tail;
    }
    literal.push_str(rest);
    out.push_str(&escape(&literal));
    (out, params)
}

/// A channel parameter name: letters, digits, `_`, `-` and `.`.
fn is_param(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

struct RequestText<'a> {
    op: &'a Op,
    kind: MessagingKind,
    synthesizer: &'a Synthesizer<'a>,
}

impl RequestText<'_> {
    fn render(&self) -> String {
        let op = self.op;
        let mut text = format!(
            "version: 1\nkind: {}\nname: {}\n",
            self.kind,
            quoted(&op.id)
        );
        if let Some(summary) = &op.summary {
            text.push_str(&format!("description: {}\n", quoted(summary)));
        }
        text.push_str(&format!("operation: {}\n", op.operation.as_str()));
        let (destination, params) = address(op, self.kind);
        text.push_str(&self.connection(&destination));
        if let Some(message) = op.messages.iter().find(|m| m.payload.is_some()) {
            text.push_str(&format!(
                "asyncapi: {{document: {}, message: {}}}\n",
                quoted(&format!("../{DOCUMENT_FILE}")),
                quoted(&message.name)
            ));
        }
        if !params.is_empty() {
            text.push_str("variables:\n");
            for param in params {
                text.push_str(&format!("  {param}: \"example\"\n"));
            }
        }
        if op.operation == Operation::Produce {
            text.push_str(&self.messages());
        }
        text
    }

    /// The broker and destination fields of the kind.
    fn connection(&self, destination: &str) -> String {
        let b = &self.op.bindings;
        let produce = self.op.operation == Operation::Produce;
        match self.kind {
            MessagingKind::Kafka => format!(
                "brokers: \"{{{{brokers}}}}\"\ntopic: {}\n",
                quoted(destination)
            ),
            MessagingKind::Mqtt => {
                let mut text = format!(
                    "broker: \"{{{{broker}}}}\"\ntopic: {}\n",
                    quoted(destination)
                );
                if let Some(qos) = b.mqtt_qos.filter(|q| *q <= 2) {
                    text.push_str(&format!("qos: {qos}\n"));
                }
                if let Some(retain) = b.mqtt_retain.filter(|_| produce) {
                    text.push_str(&format!("retain: {retain}\n"));
                }
                text
            }
            MessagingKind::Amqp => self.amqp(destination, produce),
        }
    }

    fn amqp(&self, destination: &str, produce: bool) -> String {
        let b = &self.op.bindings;
        let mut text = "broker: \"{{broker}}\"\n".to_owned();
        let exchange = b.amqp_exchange.clone();
        match (produce, exchange) {
            (true, exchange) => text.push_str(&format!(
                "exchange: {}\nrouting_key: {}\n",
                quoted(&escape(&exchange.unwrap_or_default())),
                quoted(destination)
            )),
            (false, Some(exchange)) => text.push_str(&format!(
                "exchange: {}\nbinding_key: {}\n",
                quoted(&escape(&exchange)),
                quoted(destination)
            )),
            (false, None) => {
                let queue = b
                    .amqp_queue
                    .as_deref()
                    .map_or_else(|| destination.to_owned(), escape);
                text.push_str(&format!("queue: {}\nrequeue: true\n", quoted(&queue)));
            }
        }
        text
    }

    /// One message per documented message, with its example payload.
    fn messages(&self) -> String {
        let mut text = "messages:\n".to_owned();
        if self.op.messages.is_empty() {
            text.push_str("  - value: {}\n");
        }
        for message in &self.op.messages {
            text.push_str(&format!("  - value: {}\n", inline(&self.example(message))));
            if let Some(key) = message
                .key
                .as_ref()
                .filter(|_| self.kind == MessagingKind::Kafka)
            {
                text.push_str(&format!(
                    "    key: {}\n",
                    inline(&escape_values(&self.synthesizer.value(key)))
                ));
            }
        }
        text
    }

    /// The example payload, escaped: examples and schema defaults come
    /// from the document.
    fn example(&self, message: &Message) -> Value {
        let value = match (&message.example, &message.payload) {
            (Some(example), _) => example.clone(),
            (None, Some(schema)) => self.synthesizer.value(schema),
            (None, None) => json!({}),
        };
        escape_values(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(address: &str, operation: Operation) -> Op {
        Op {
            id: "o".into(),
            operation,
            summary: None,
            channel: "c".into(),
            address: address.into(),
            servers: Vec::new(),
            messages: Vec::new(),
            bindings: spec::Bindings::default(),
        }
    }

    #[test]
    fn channel_parameters_become_variables_or_wildcards() {
        let produce = op("sensors/{room}/temp", Operation::Produce);
        assert_eq!(
            address(&produce, MessagingKind::Mqtt),
            ("sensors/{{room}}/temp".to_owned(), vec!["room".to_owned()])
        );
        let consume = op("sensors/{room}/temp", Operation::Consume);
        assert_eq!(
            address(&consume, MessagingKind::Mqtt),
            ("sensors/+/temp".to_owned(), vec![])
        );
        assert_eq!(
            address(&consume, MessagingKind::Kafka).0,
            "sensors/{{room}}/temp"
        );
    }

    #[test]
    fn document_literals_cannot_plant_template_references() {
        let cases = [
            (
                "{{secret:env:HOME}}/{room}",
                "\\{{secret:env:HOME}}/{{room}}",
            ),
            ("a/{{kafka_pw}}", "a/\\{{kafka_pw}}"),
            ("a/{x:y}/{", "a/{x:y}/{"),
        ];
        for (raw, expected) in cases {
            let produce = op(raw, Operation::Produce);
            assert_eq!(address(&produce, MessagingKind::Kafka).0, expected, "{raw}");
        }
        assert_eq!(
            address(
                &op("a/{{kafka_pw}}", Operation::Produce),
                MessagingKind::Kafka
            )
            .1,
            Vec::<String>::new()
        );
    }

    #[test]
    fn names_are_unique_kebab_case() {
        let mut taken = Vec::new();
        assert_eq!(unique(name("sendOrder"), &mut taken), "send-order");
        assert_eq!(unique(name("send_order"), &mut taken), "send-order-2");
        assert_eq!(name("!!"), "unnamed");
    }
}
