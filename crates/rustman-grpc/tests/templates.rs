//! The browser API and message templates (add-grpc-client task 2.4):
//! snapshots of templates covering nested (to depth 3), repeated, enum,
//! oneof and map fields, and of `describe` output.

use prost_reflect::DescriptorPool;
use rustman_grpc::{CallType, Description, Schema, SchemaOrigin, TEMPLATE_DEPTH, template};
use rustman_test_support::grpc::descriptor_set;

fn schema() -> Schema {
    let pool = DescriptorPool::decode(descriptor_set().as_slice()).unwrap();
    Schema::new(pool, SchemaOrigin::DescriptorSet)
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap()
}

#[test]
fn product_template() {
    let schema = schema();
    let product = schema.pool.get_message_by_name("shop.v1.Product").unwrap();
    let value = template(&product);
    // Nested messages stop expanding below depth 3.
    assert_eq!(TEMPLATE_DEPTH, 3);
    assert_eq!(
        value["dimensions"]["width"]["detail"]["note"],
        serde_json::json!({})
    );
    insta::assert_snapshot!(pretty(&value));
}

#[test]
fn every_field_kind() {
    let schema = schema();
    let product = schema.pool.get_message_by_name("shop.v1.Product").unwrap();
    let value = template(&product);
    // Repeated: one element; enum: its first value name; map: one entry;
    // oneof: only its first field; well-known types in canonical JSON.
    assert_eq!(value["tags"], serde_json::json!([""]));
    assert_eq!(value["variants"][0]["sku"], "");
    assert_eq!(value["category"], "CATEGORY_UNSPECIFIED");
    assert_eq!(value["stockByStore"], serde_json::json!({"": 0}));
    assert_eq!(value["inStock"], 0);
    assert!(value.get("restockAt").is_none());
    assert_eq!(value["createdAt"], "1970-01-01T00:00:00Z");
    assert_eq!(value["views"], "0");
    assert_eq!(value["price"]["units"], "0");
}

#[test]
fn request_templates() {
    let schema = schema();
    for name in ["shop.v1.GetProductRequest", "shop.v1.ListProductsRequest"] {
        let message = schema.pool.get_message_by_name(name).unwrap();
        insta::assert_snapshot!(name, pretty(&template(&message)));
    }
}

/// Spec "Describe a method".
#[test]
fn describe_a_server_streaming_method() {
    let described = schema().describe("shop.v1.Catalog/ListProducts").unwrap();
    let Description::Method { method, .. } = &described else {
        panic!("a method");
    };
    assert_eq!(method.call_type, CallType::ServerStreaming);
    let text = described.to_string();
    assert!(text.contains("call type: server streaming"), "{text}");
    insta::assert_snapshot!(text);
}

#[test]
fn describe_services_and_messages() {
    let schema = schema();
    insta::assert_snapshot!(
        "service",
        schema.describe("shop.v1.Catalog").unwrap().to_string()
    );
    insta::assert_snapshot!(
        "message",
        schema.describe("shop.v1.Variant").unwrap().to_string()
    );
    let err = schema.describe("shop.v1.Nope").unwrap_err();
    assert_eq!(err.to_string(), "grpc: unknown symbol 'shop.v1.Nope'");
    assert!(schema.method("shop.v1.Catalog.GetProduct").is_ok());
    assert!(schema.method("shop.v1.Catalog/Nope").is_err());
}

#[test]
fn services_list_with_call_types() {
    let schema = schema();
    let services = schema.services();
    let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["shop.v1.Catalog"]);
    let labels: Vec<&str> = services[0]
        .methods
        .iter()
        .map(|m| m.call_type.label())
        .collect();
    assert_eq!(
        labels,
        ["unary", "server streaming", "client streaming", "bidi"]
    );
}
