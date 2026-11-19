//! The proto3 JSON mapping of messages (spec "Message mapping"), through
//! prost-reflect's serde support: field names in lowerCamelCase or as
//! written in the `.proto`, enums by name or number, 64-bit integers as
//! strings or numbers, and well-known types in their canonical forms.
//!
//! Unknown fields are refused before anything is sent, naming the field and
//! the message type it is missing from.

use prost_reflect::{
    DeserializeOptions, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, SerializeOptions,
};
use serde_json::Value;

use crate::error::GrpcError;

/// `value` encoded as a message of type `message`.
pub fn to_message(message: &MessageDescriptor, value: &Value) -> Result<DynamicMessage, GrpcError> {
    check_fields(message, value)?;
    let options = DeserializeOptions::new().deny_unknown_fields(true);
    DynamicMessage::deserialize_with_options(message.clone(), value, &options)
        .map_err(|e| GrpcError::Message(format!("invalid {} message: {e}", message.full_name())))
}

/// `message` in the canonical proto3 JSON form: default values left out,
/// 64-bit integers as strings, enums by name.
pub fn to_json(message: &DynamicMessage) -> Value {
    let options = SerializeOptions::new();
    message
        .serialize_with_options(serde_json::value::Serializer, &options)
        .unwrap_or(Value::Null)
}

/// Every key of `value` (and of the messages nested in it) must name a
/// field of `message`. Well-known types have their own JSON forms and are
/// left to the deserializer.
fn check_fields(message: &MessageDescriptor, value: &Value) -> Result<(), GrpcError> {
    let Value::Object(map) = value else {
        return Ok(());
    };
    if is_well_known(message) {
        return Ok(());
    }
    for (key, item) in map {
        let field = message
            .get_field_by_json_name(key)
            .or_else(|| message.get_field_by_name(key))
            .ok_or_else(|| GrpcError::UnknownField {
                field: key.clone(),
                message_type: message.full_name().to_owned(),
            })?;
        check_field(&field, item)?;
    }
    Ok(())
}

fn check_field(field: &FieldDescriptor, value: &Value) -> Result<(), GrpcError> {
    let Kind::Message(message) = field.kind() else {
        return Ok(());
    };
    if field.is_map() {
        return check_map_values(&message, value);
    }
    match value {
        Value::Array(items) if field.is_list() => items
            .iter()
            .try_for_each(|item| check_fields(&message, item)),
        _ => check_fields(&message, value),
    }
}

/// The values of a map field whose values are messages.
fn check_map_values(entry: &MessageDescriptor, value: &Value) -> Result<(), GrpcError> {
    let Kind::Message(message) = entry.map_entry_value_field().kind() else {
        return Ok(());
    };
    let Value::Object(map) = value else {
        return Ok(());
    };
    map.values()
        .try_for_each(|item| check_fields(&message, item))
}

fn is_well_known(message: &MessageDescriptor) -> bool {
    message.package_name() == "google.protobuf"
}

#[cfg(test)]
mod tests {
    use prost::Message as _;
    use prost_reflect::DescriptorPool;
    use serde_json::json;

    use super::*;

    fn pool() -> DescriptorPool {
        DescriptorPool::decode(rustman_test_support::grpc::descriptor_set().as_slice()).unwrap()
    }

    fn message(name: &str) -> MessageDescriptor {
        pool().get_message_by_name(name).unwrap()
    }

    #[test]
    fn unknown_fields_are_named_with_their_type() {
        let err = to_message(
            &message("shop.v1.GetProductRequest"),
            &json!({"idd": "p-1"}),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "grpc: unknown field 'idd' in shop.v1.GetProductRequest"
        );
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn unknown_fields_in_nested_repeated_and_map_messages() {
        let product = message("shop.v1.Product");
        let err = to_message(&product, &json!({"price": {"currency": "EUR"}})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "grpc: unknown field 'currency' in shop.v1.Money"
        );
        let err =
            to_message(&product, &json!({"variants": [{"sku": "a", "colour": 1}]})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "grpc: unknown field 'colour' in shop.v1.Variant"
        );
        let err = to_message(
            &product,
            &json!({"dimensions": {"width": {"detail": {"note": {"txt": "x"}}}}}),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("in shop.v1.Dimensions.Measure.Detail.Note"),
            "{err}"
        );
    }

    #[test]
    fn timestamps_encode_to_seconds_and_nanos() {
        let product = message("shop.v1.Product");
        let encoded = to_message(&product, &json!({"createdAt": "2026-11-17T10:00:00Z"}))
            .unwrap()
            .encode_to_vec();
        let decoded = DynamicMessage::decode(product, encoded.as_slice()).unwrap();
        let created = decoded.get_field_by_name("created_at").unwrap();
        let created = created.as_message().unwrap();
        let seconds = created.get_field_by_name("seconds").unwrap().as_i64();
        let nanos = created.get_field_by_name("nanos").unwrap().as_i32();
        assert_eq!(seconds, Some(1_794_909_600));
        assert_eq!(nanos, Some(0));
    }

    #[test]
    fn names_enums_and_64_bit_integers_take_every_form() {
        let product = message("shop.v1.Product");
        let camel = to_message(
            &product,
            &json!({"stockByStore": {"a": 1}, "category": "BOOKS", "views": "12"}),
        )
        .unwrap();
        let original = to_message(
            &product,
            &json!({"stock_by_store": {"a": 1}, "category": 1, "views": 12}),
        )
        .unwrap();
        assert_eq!(camel, original);
        assert_eq!(
            to_json(&camel),
            json!({"stockByStore": {"a": 1}, "category": "BOOKS", "views": "12"})
        );
    }

    #[test]
    fn durations_and_type_errors() {
        let product = message("shop.v1.Product");
        let err = to_message(&product, &json!({"views": "many"})).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("grpc: invalid shop.v1.Product message:"),
            "{err}"
        );
        let ok = to_message(&product, &json!({"restockAt": "2026-01-01T00:00:00.5Z"})).unwrap();
        assert_eq!(
            to_json(&ok),
            json!({"restockAt": "2026-01-01T00:00:00.500Z"})
        );
    }
}
