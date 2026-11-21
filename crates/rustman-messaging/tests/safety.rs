//! Safety classification of messaging operations (add-messaging-client
//! task 5.3): produce is a mutation, a named-queue consume without
//! requeue is destructive, every other consume is a read.

mod common;

use rustman_core::safety::{MethodClass, Permission};
use rustman_messaging::safety::{Refusal, check, classify};

fn amqp(body: &str) -> String {
    format!("version: 1\nkind: amqp\nname: a\nbroker: amqp://127.0.0.1:5672\n{body}")
}

#[test]
fn operations_map_to_policy_classes() {
    let cases = [
        (
            "version: 1\nkind: kafka\nname: k\noperation: produce\nbrokers: h:9092\ntopic: t\nmessages: [{value: x}]\n".to_owned(),
            MethodClass::Mutation,
        ),
        (
            "version: 1\nkind: kafka\nname: k\noperation: consume\nbrokers: h:9092\ntopic: t\n".to_owned(),
            MethodClass::ReadOnly,
        ),
        (
            "version: 1\nkind: mqtt\nname: m\noperation: produce\nbroker: mqtt://h\ntopic: t\nmessages: [{value: x}]\n".to_owned(),
            MethodClass::Mutation,
        ),
        (amqp("operation: consume\nexchange: orders\nbinding_key: '#'\n"), MethodClass::ReadOnly),
        (amqp("operation: consume\nqueue: payments\ndestructive: true\n"), MethodClass::Destructive),
        (amqp("operation: consume\nqueue: payments\nrequeue: true\n"), MethodClass::ReadOnly),
    ];
    for (text, class) in cases {
        assert_eq!(classify(&common::resolve(&text, "")), class, "{text}");
    }
}

#[test]
fn agents_need_the_permission_of_the_class() {
    let produce = common::resolve(
        "version: 1\nkind: kafka\nname: k\noperation: produce\nbrokers: h:9092\ntopic: t\nmessages: [{value: x}]\n",
        "",
    );
    let refusal = check(Permission::ReadOnly, &produce).unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "blocked: produce requires allow_mutations"
    );
    assert_eq!(refusal, Refusal("produce requires allow_mutations"));
    assert_eq!(check(Permission::Mutations, &produce), Ok(()));
    let drain = common::resolve(
        &amqp("operation: consume\nqueue: payments\ndestructive: true\n"),
        "",
    );
    assert_eq!(
        check(Permission::Mutations, &drain).unwrap_err().reason(),
        "consuming from an existing queue requires allow_destructive"
    );
    assert_eq!(check(Permission::Destructive, &drain), Ok(()));
}

#[test]
fn a_persistent_mqtt_session_consume_is_destructive() {
    let resume = common::resolve(
        "version: 1\nkind: mqtt\nname: m\noperation: consume\nbroker: mqtt://h\ntopic: t/#\nclient_id: probe-1\nclean_session: false\n",
        "",
    );
    assert_eq!(classify(&resume), MethodClass::Destructive);
    assert_eq!(
        check(Permission::Mutations, &resume).unwrap_err().reason(),
        "consuming through a persistent session (clean_session: false) requires allow_destructive"
    );
    assert_eq!(check(Permission::Destructive, &resume), Ok(()));
    let fresh = common::resolve(
        "version: 1\nkind: mqtt\nname: m\noperation: consume\nbroker: mqtt://h\ntopic: t/#\nclient_id: probe-1\n",
        "",
    );
    assert_eq!(classify(&fresh), MethodClass::ReadOnly);
}

#[test]
fn the_schema_registry_is_an_egress_target_of_kafka_requests() {
    let kafka = common::resolve(
        "version: 1\nkind: kafka\nname: k\noperation: consume\nbrokers: h:9092\ntopic: t\nschema_registry: {url: 'http://registry.example:8081'}\n",
        "",
    );
    assert_eq!(
        rustman_messaging::safety::registry_url(&kafka),
        Some("http://registry.example:8081")
    );
    let plain = common::resolve(
        "version: 1\nkind: kafka\nname: k\noperation: consume\nbrokers: h:9092\ntopic: t\n",
        "",
    );
    assert_eq!(rustman_messaging::safety::registry_url(&plain), None);
}
