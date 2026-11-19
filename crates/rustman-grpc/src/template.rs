//! Message templates (spec "Service and method browser"): every field at
//! its default in the proto3 JSON mapping. Nested messages are expanded to
//! [`TEMPLATE_DEPTH`] levels (deeper ones are `{}`), repeated fields hold
//! one element, maps one entry, enums their first value name, and a oneof
//! only its first field. Well-known types take their canonical JSON form.

use prost_reflect::{FieldDescriptor, Kind, MessageDescriptor};
use serde_json::{Map, Value, json};

/// How many levels of nested messages a template expands.
pub const TEMPLATE_DEPTH: usize = 3;

/// The template of `message`.
pub fn template(message: &MessageDescriptor) -> Value {
    message_value(message, 0)
}

fn message_value(message: &MessageDescriptor, depth: usize) -> Value {
    if let Some(value) = well_known(message) {
        return value;
    }
    let mut object = Map::new();
    let mut oneofs: Vec<String> = Vec::new();
    for field in message.fields() {
        if let Some(oneof) = field.containing_oneof().filter(|o| !o.is_synthetic()) {
            if oneofs.iter().any(|name| name == oneof.name()) {
                continue;
            }
            oneofs.push(oneof.name().to_owned());
        }
        object.insert(field.json_name().to_owned(), field_value(&field, depth));
    }
    Value::Object(object)
}

fn field_value(field: &FieldDescriptor, depth: usize) -> Value {
    if field.is_map() {
        let Kind::Message(entry) = field.kind() else {
            return json!({});
        };
        let key = map_key(&entry.map_entry_key_field().kind());
        let value = single(&entry.map_entry_value_field().kind(), depth);
        return json!({ key: value });
    }
    let value = single(&field.kind(), depth);
    if field.is_list() {
        Value::Array(vec![value])
    } else {
        value
    }
}

/// The default of one value of `kind`; 64-bit integers are strings, as
/// the proto3 JSON mapping writes them.
fn single(kind: &Kind, depth: usize) -> Value {
    match kind {
        Kind::Double | Kind::Float => json!(0.0),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => json!("0"),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 | Kind::Uint32 | Kind::Fixed32 => json!(0),
        Kind::Bool => json!(false),
        Kind::String | Kind::Bytes => json!(""),
        Kind::Enum(e) => e.values().next().map_or(json!(0), |v| json!(v.name())),
        Kind::Message(m) if depth < TEMPLATE_DEPTH => message_value(m, depth + 1),
        Kind::Message(m) => well_known(m).unwrap_or_else(|| json!({})),
    }
}

/// A map key as JSON writes it: always a string.
fn map_key(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "false".to_owned(),
        Kind::String => String::new(),
        _ => "0".to_owned(),
    }
}

/// The canonical JSON default of a well-known type.
fn well_known(message: &MessageDescriptor) -> Option<Value> {
    let value = match message.full_name() {
        "google.protobuf.Timestamp" => json!("1970-01-01T00:00:00Z"),
        "google.protobuf.Duration" => json!("0s"),
        "google.protobuf.FieldMask" => json!(""),
        "google.protobuf.Struct" | "google.protobuf.Empty" => json!({}),
        "google.protobuf.Value" => Value::Null,
        "google.protobuf.ListValue" => json!([]),
        "google.protobuf.Any" => json!({"@type": ""}),
        name if name.starts_with("google.protobuf.") && name.ends_with("Value") => {
            let inner = message.get_field_by_name("value")?;
            single(&inner.kind(), TEMPLATE_DEPTH)
        }
        _ => return None,
    };
    Some(value)
}
