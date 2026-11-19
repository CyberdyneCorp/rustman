//! The service and method browser (spec "Service and method browser"):
//! services with their methods and call types, and descriptions of a
//! service, method or message with a request template.

use std::fmt;

use prost_reflect::{
    FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor, ServiceDescriptor,
};
use serde::Serialize;
use serde_json::Value;

use crate::error::GrpcError;
use crate::schema::Schema;
use crate::template::template;

/// The four gRPC call types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallType {
    Unary,
    ServerStreaming,
    ClientStreaming,
    Bidi,
}

impl CallType {
    pub fn of(method: &MethodDescriptor) -> Self {
        match (method.is_client_streaming(), method.is_server_streaming()) {
            (false, false) => Self::Unary,
            (false, true) => Self::ServerStreaming,
            (true, false) => Self::ClientStreaming,
            (true, true) => Self::Bidi,
        }
    }

    /// `unary`, `server streaming`, `client streaming` or `bidi`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unary => "unary",
            Self::ServerStreaming => "server streaming",
            Self::ClientStreaming => "client streaming",
            Self::Bidi => "bidi",
        }
    }
}

impl fmt::Display for CallType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A service and its methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceInfo {
    /// Fully qualified, such as `shop.v1.Catalog`.
    pub name: String,
    pub methods: Vec<MethodInfo>,
}

/// A method: its path, call type and message types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MethodInfo {
    /// `package.Service/Method`.
    pub path: String,
    pub name: String,
    pub call_type: CallType,
    pub request: String,
    pub response: String,
}

/// `IdempotencyLevel.NO_SIDE_EFFECTS` of `google.protobuf.MethodOptions`.
const NO_SIDE_EFFECTS: i32 = 1;

/// Whether the proto marks `method` as a read:
/// `option idempotency_level = NO_SIDE_EFFECTS;`.
pub fn has_no_side_effects(method: &MethodDescriptor) -> bool {
    let options = method.options();
    let level = options.get_field_by_name("idempotency_level");
    matches!(
        level.as_deref(),
        Some(prost_reflect::Value::EnumNumber(NO_SIDE_EFFECTS))
    )
}

impl MethodInfo {
    fn of(method: &MethodDescriptor) -> Self {
        Self {
            path: format!("{}/{}", method.parent_service().full_name(), method.name()),
            name: method.name().to_owned(),
            call_type: CallType::of(method),
            request: method.input().full_name().to_owned(),
            response: method.output().full_name().to_owned(),
        }
    }
}

impl ServiceInfo {
    fn of(service: &ServiceDescriptor) -> Self {
        Self {
            name: service.full_name().to_owned(),
            methods: service.methods().map(|m| MethodInfo::of(&m)).collect(),
        }
    }
}

/// A message type: its fields and a template.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessageInfo {
    pub name: String,
    pub fields: Vec<FieldInfo>,
    pub template: Value,
}

/// One field of a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldInfo {
    pub name: String,
    pub json_name: String,
    pub number: u32,
    /// `string`, `int64`, an enum or message name, or `map<K, V>`.
    pub type_name: String,
    pub repeated: bool,
    /// The oneof the field belongs to, if any (proto3 `optional` aside).
    pub oneof: Option<String>,
}

impl MessageInfo {
    fn of(message: &MessageDescriptor) -> Self {
        Self {
            name: message.full_name().to_owned(),
            fields: message.fields().map(|f| FieldInfo::of(&f)).collect(),
            template: template(message),
        }
    }
}

impl FieldInfo {
    fn of(field: &FieldDescriptor) -> Self {
        let type_name = match field.kind() {
            Kind::Message(entry) if field.is_map() => format!(
                "map<{}, {}>",
                kind_name(&entry.map_entry_key_field().kind()),
                kind_name(&entry.map_entry_value_field().kind())
            ),
            kind => kind_name(&kind),
        };
        Self {
            name: field.name().to_owned(),
            json_name: field.json_name().to_owned(),
            number: field.number(),
            type_name,
            repeated: field.is_list(),
            oneof: field
                .containing_oneof()
                .filter(|o| !o.is_synthetic())
                .map(|o| o.name().to_owned()),
        }
    }
}

fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Message(m) => m.full_name().to_owned(),
        Kind::Enum(e) => e.full_name().to_owned(),
        scalar => format!("{scalar:?}").to_ascii_lowercase(),
    }
}

/// What `describe` shows for a symbol.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Description {
    Service(ServiceInfo),
    Method {
        method: MethodInfo,
        /// The template of the request message.
        template: Value,
    },
    Message(MessageInfo),
}

impl Schema {
    /// Every service, sorted by name, with its methods in file order.
    pub fn services(&self) -> Vec<ServiceInfo> {
        let mut services: Vec<ServiceInfo> =
            self.pool.services().map(|s| ServiceInfo::of(&s)).collect();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        services
    }

    /// The method `path` names: `package.Service/Method` (or with a `.`
    /// before the method name).
    pub fn method(&self, path: &str) -> Result<MethodDescriptor, GrpcError> {
        let unknown = || GrpcError::UnknownSymbol(path.to_owned());
        let (service, name) = path
            .trim_start_matches('/')
            .rsplit_once(['/', '.'])
            .ok_or_else(unknown)?;
        let service = self.pool.get_service_by_name(service).ok_or_else(unknown)?;
        let method = service.methods().find(|m| m.name() == name);
        method.ok_or_else(unknown)
    }

    /// Describe a service, method or message.
    pub fn describe(&self, symbol: &str) -> Result<Description, GrpcError> {
        let symbol = symbol.trim().trim_start_matches('.');
        if let Some(service) = self.pool.get_service_by_name(symbol) {
            return Ok(Description::Service(ServiceInfo::of(&service)));
        }
        if let Some(message) = self.pool.get_message_by_name(symbol) {
            return Ok(Description::Message(MessageInfo::of(&message)));
        }
        let method = self.method(symbol)?;
        Ok(Description::Method {
            template: template(&method.input()),
            method: MethodInfo::of(&method),
        })
    }
}

impl fmt::Display for Description {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(service) => write_service(f, service),
            Self::Method { method, template } => {
                writeln!(f, "{}", method.path)?;
                writeln!(f, "  call type: {}", method.call_type)?;
                writeln!(f, "  request:   {}", method.request)?;
                writeln!(f, "  response:  {}", method.response)?;
                writeln!(f, "  request template:")?;
                write_json(f, template)
            }
            Self::Message(message) => {
                writeln!(f, "message {}", message.name)?;
                for field in &message.fields {
                    writeln!(f, "  {}", field_line(field))?;
                }
                writeln!(f, "  template:")?;
                write_json(f, &message.template)
            }
        }
    }
}

fn write_service(f: &mut fmt::Formatter<'_>, service: &ServiceInfo) -> fmt::Result {
    writeln!(f, "service {}", service.name)?;
    for m in &service.methods {
        writeln!(
            f,
            "  {} ({}): {} -> {}",
            m.name, m.call_type, m.request, m.response
        )?;
    }
    Ok(())
}

fn field_line(field: &FieldInfo) -> String {
    let repeated = if field.repeated { "repeated " } else { "" };
    let oneof = field
        .oneof
        .as_ref()
        .map(|o| format!(" (oneof {o})"))
        .unwrap_or_default();
    format!(
        "{repeated}{} {} = {};{oneof}",
        field.type_name, field.name, field.number
    )
}

/// `value` as indented JSON, each line indented by four spaces.
fn write_json(f: &mut fmt::Formatter<'_>, value: &Value) -> fmt::Result {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    for line in text.lines() {
        writeln!(f, "    {line}")?;
    }
    Ok(())
}
