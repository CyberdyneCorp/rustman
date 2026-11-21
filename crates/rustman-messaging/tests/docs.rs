//! The examples of docs/messaging.md (add-messaging-client task 5.5):
//! every full request file loads, with the fields the docs describe, and
//! the collection example runs its consume before the HTTP request.

use std::path::Path;

use rustman_core::model::{
    Assertion, KindRegistry, MessagingKind, MessagingRequest, Operation, Request, Start,
};

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// Every fenced `yaml` block of `text` that is a whole request file.
fn request_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim_end() == "```yaml" {
            let block: String = lines
                .by_ref()
                .take_while(|l| l.trim_end() != "```")
                .map(|l| format!("{l}\n"))
                .collect();
            if block.starts_with("version: 1\n") {
                out.push(block);
            }
        }
    }
    out
}

fn load(text: &str) -> Request {
    KindRegistry::default()
        .load(Path::new("example.rman.yaml"), text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn messaging(text: &str) -> MessagingRequest {
    match load(text) {
        Request::Messaging(m) => *m,
        other => panic!("a messaging request, got {}", other.kind()),
    }
}

#[test]
fn every_request_file_example_loads() {
    let blocks = request_blocks(&doc("messaging.md"));
    let kinds: Vec<String> = blocks.iter().map(|b| load(b).kind().to_owned()).collect();
    assert_eq!(kinds, ["kafka", "mqtt", "amqp", "kafka", "http"]);
}

#[test]
fn the_examples_read_as_documented() {
    let blocks = request_blocks(&doc("messaging.md"));
    let produce = messaging(&blocks[0]);
    assert_eq!(produce.messaging_kind(), MessagingKind::Kafka);
    assert_eq!(produce.effective_operation(), Operation::Produce);
    assert_eq!(produce.partition, Some(0));
    let mqtt = messaging(&blocks[1]);
    assert_eq!(mqtt.messaging_kind(), MessagingKind::Mqtt);
    let amqp = messaging(&blocks[2]);
    assert_eq!(amqp.mandatory, Some(true));
    check_events_example(&messaging(&blocks[3]));
}

/// The `before_next` consume example with its assertions and capture.
fn check_events_example(events: &MessagingRequest) {
    assert_eq!(events.effective_operation(), Operation::Consume);
    assert_eq!(events.effective_start(), Start::BeforeNext);
    assert_eq!(events.effective_limits().effective_max_messages(), 1);
    let assertions = events.assertions.as_ref().unwrap();
    assert!(matches!(assertions[0].get(), Assertion::Messages(_)));
    assert!(matches!(assertions[1].get(), Assertion::Json(_)));
    assert!(matches!(assertions[2].get(), Assertion::Header(_)));
    assert_eq!(events.captures.as_ref().unwrap().len(), 1);
}
