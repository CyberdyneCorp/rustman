//! Registry schemas and their payload codecs (design D4): Avro with
//! `apache-avro`, Protobuf with `prost-reflect` over descriptors compiled
//! from the registry's `.proto` text by `protox`, and JSON Schema
//! payloads as plain JSON.

use apache_avro::reader::datum::GenericDatumReader;
use apache_avro::types::Value as AvroValue;
use apache_avro::writer::datum::GenericDatumWriter;
use prost_reflect::prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};
use serde_json::Value;

use super::wire;

/// The file name a registry `.proto` schema is compiled as.
const PROTO_FILE: &str = "registry.proto";

/// A schema fetched from the registry, ready to decode and encode.
#[derive(Debug)]
pub enum RegistrySchema {
    Avro(Box<apache_avro::Schema>),
    Protobuf(DescriptorPool),
    Json,
}

impl RegistrySchema {
    /// Parse `text` of registry `schema_type` (`AVRO` when absent).
    pub fn parse(schema_type: Option<&str>, text: &str) -> Result<Self, String> {
        match schema_type.unwrap_or("AVRO") {
            "AVRO" => apache_avro::Schema::parse_str(text)
                .map(|s| Self::Avro(Box::new(s)))
                .map_err(|e| format!("invalid Avro schema: {e}")),
            "PROTOBUF" => compile_proto(text).map(Self::Protobuf),
            "JSON" => Ok(Self::Json),
            other => Err(format!("unsupported schema type '{other}'")),
        }
    }

    /// The schema type as the registry names it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Avro(_) => "AVRO",
            Self::Protobuf(_) => "PROTOBUF",
            Self::Json => "JSON",
        }
    }

    /// The payload after the wire-format header, as JSON.
    pub fn decode(&self, payload: &[u8]) -> Result<Value, String> {
        match self {
            Self::Avro(schema) => decode_avro(schema, payload),
            Self::Protobuf(pool) => decode_proto(pool, payload),
            Self::Json => serde_json::from_slice(payload).map_err(|e| format!("invalid JSON: {e}")),
        }
    }

    /// `value` encoded for this schema, without the wire-format header.
    pub fn encode(&self, value: &Value) -> Result<Vec<u8>, String> {
        match self {
            Self::Avro(schema) => encode_avro(schema, value),
            Self::Protobuf(pool) => encode_proto(pool, value),
            Self::Json => Ok(crate::payload::compact_json(value)),
        }
    }
}

fn decode_avro(schema: &apache_avro::Schema, mut payload: &[u8]) -> Result<Value, String> {
    let invalid = |e: apache_avro::Error| format!("invalid Avro payload: {e}");
    let reader = GenericDatumReader::builder(schema)
        .build()
        .map_err(invalid)?;
    let value = reader.read_value(&mut payload).map_err(invalid)?;
    Value::try_from(value).map_err(|e| format!("Avro value has no JSON form: {e}"))
}

fn encode_avro(schema: &apache_avro::Schema, value: &Value) -> Result<Vec<u8>, String> {
    let invalid = |e: apache_avro::Error| format!("value does not match the Avro schema: {e}");
    let avro = AvroValue::try_from(value.clone()).map_err(invalid)?;
    let resolved = avro.resolve(schema).map_err(invalid)?;
    let writer = GenericDatumWriter::builder(schema)
        .build()
        .map_err(invalid)?;
    writer.write_value_to_vec(resolved).map_err(invalid)
}

/// Compile a registry `.proto` schema; imports resolve only to the
/// well-known Google types.
fn compile_proto(text: &str) -> Result<DescriptorPool, String> {
    let mut resolver = ChainFileResolver::new();
    resolver.add(Single(text.to_owned()));
    resolver.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(resolver);
    compiler
        .open_file(PROTO_FILE)
        .map_err(|e| format!("invalid Protobuf schema: {e}"))?;
    Ok(compiler.descriptor_pool())
}

/// The registry schema text as the one file to compile.
struct Single(String);

impl FileResolver for Single {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        if name == PROTO_FILE {
            File::from_source(name, &self.0)
        } else {
            Err(protox::Error::file_not_found(name))
        }
    }
}

/// The message at `path` (indexes into top-level, then nested messages).
fn message_at(pool: &DescriptorPool, path: &[usize]) -> Option<MessageDescriptor> {
    let file = pool.get_file_by_name(PROTO_FILE)?;
    let (first, nested) = path.split_first()?;
    let mut message = file.messages().nth(*first)?;
    for index in nested {
        let child = message.child_messages().nth(*index)?;
        message = child;
    }
    Some(message)
}

fn decode_proto(pool: &DescriptorPool, payload: &[u8]) -> Result<Value, String> {
    let (path, body) = wire::message_indexes(payload).ok_or("invalid Protobuf message indexes")?;
    let descriptor = message_at(pool, &path)
        .ok_or_else(|| format!("no message at index path {path:?} in the schema"))?;
    let message = DynamicMessage::decode(descriptor, body)
        .map_err(|e| format!("invalid Protobuf payload: {e}"))?;
    serde_json::to_value(&message).map_err(|e| format!("Protobuf value has no JSON form: {e}"))
}

/// `value` as the schema's first message, after the `[0]` index path.
fn encode_proto(pool: &DescriptorPool, value: &Value) -> Result<Vec<u8>, String> {
    let descriptor = message_at(pool, &[0]).ok_or("the Protobuf schema has no message")?;
    let message = DynamicMessage::deserialize(descriptor, value)
        .map_err(|e| format!("value does not match the Protobuf schema: {e}"))?;
    let mut bytes = vec![0];
    bytes.extend(message.encode_to_vec());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ORDER_AVRO: &str = r#"{"type":"record","name":"Order","fields":[{"name":"id","type":"int"},{"name":"total","type":"double"}]}"#;
    const ORDER_PROTO: &str =
        "syntax = \"proto3\";\nmessage Order { int32 id = 1; string note = 2; }\n";

    #[test]
    fn avro_round_trips_through_json() {
        let schema = RegistrySchema::parse(None, ORDER_AVRO).unwrap();
        assert_eq!(schema.type_name(), "AVRO");
        let bytes = schema.encode(&json!({"id": 1, "total": 9.5})).unwrap();
        assert_eq!(
            schema.decode(&bytes).unwrap(),
            json!({"id": 1, "total": 9.5})
        );
        let err = schema.encode(&json!({"id": "x"})).unwrap_err();
        assert!(
            err.starts_with("value does not match the Avro schema"),
            "{err}"
        );
        assert!(schema.decode(&[0xff]).is_err());
    }

    #[test]
    fn protobuf_round_trips_through_json() {
        let schema = RegistrySchema::parse(Some("PROTOBUF"), ORDER_PROTO).unwrap();
        let bytes = schema.encode(&json!({"id": 7, "note": "hi"})).unwrap();
        assert_eq!(bytes[0], 0, "the [0] index path");
        assert_eq!(
            schema.decode(&bytes).unwrap(),
            json!({"id": 7, "note": "hi"})
        );
        let err = schema.decode(&[2, 2, 0]).unwrap_err();
        assert!(err.contains("no message at index path [1]"), "{err}");
        let err = RegistrySchema::parse(Some("PROTOBUF"), "message {").unwrap_err();
        assert!(err.starts_with("invalid Protobuf schema"), "{err}");
    }

    #[test]
    fn json_schema_payloads_are_json() {
        let schema = RegistrySchema::parse(Some("JSON"), "{}").unwrap();
        assert_eq!(schema.decode(br#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(schema.encode(&json!({"a": 1})).unwrap(), br#"{"a":1}"#);
        assert!(RegistrySchema::parse(Some("XML"), "").is_err());
    }
}
