//! The `kind: grpc` examples in docs/request-files.md load (add-grpc-client
//! task 5.3), with the fields they show.

use std::path::Path;

use rustman_core::model::{Assertion, GrpcRequest, KindRegistry, Request};
use serde_json::json;

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// Every fenced `yaml` block of `text` that is a grpc request file.
fn request_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim_end() == "```yaml" {
            let block: Vec<&str> = lines
                .by_ref()
                .take_while(|l| l.trim_end() != "```")
                .collect();
            let text: String = block.iter().map(|l| format!("{l}\n")).collect();
            if text.contains("kind: grpc") {
                out.push(text);
            }
        }
    }
    out
}

/// The gRPC section of request-files.md.
fn section() -> String {
    let text = doc("request-files.md");
    let start = text.find("## gRPC requests").expect("the section");
    let rest = &text[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    rest[..end].to_owned()
}

fn load(text: &str) -> GrpcRequest {
    let request = KindRegistry::default()
        .load(Path::new("example.rman.yaml"), text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
    let Request::Grpc(grpc) = request else {
        panic!("a grpc request");
    };
    *grpc
}

#[test]
fn the_request_file_examples_load() {
    let blocks = request_blocks(&section());
    assert_eq!(blocks.len(), 3);

    let unary = load(&blocks[0]);
    assert_eq!(unary.method, "shop.v1.Catalog/GetProduct");
    assert_eq!(unary.message_list(), [&json!({"id": "p-1"})]);
    assert_eq!(unary.deadline_ms, Some(5000));
    let metadata = unary.metadata.as_deref().unwrap();
    assert_eq!(metadata[0].value, "Bearer {{token}}");
    assert!(matches!(
        unary.assertion_list()[0],
        Assertion::GrpcStatus(s) if s.code == 0
    ));
    assert_eq!(unary.captures.as_ref().unwrap().len(), 1);

    let list = load(&blocks[1]);
    assert_eq!(list.effective_stream().effective_max_messages(), 3);
    let schema = list.schema.as_ref().unwrap();
    assert_eq!(
        schema.protos.as_ref().unwrap()[0].get(),
        "protos/shop/v1/catalog.proto"
    );

    let upload = load(&blocks[2]);
    assert!(upload.has_message_list());
    assert_eq!(upload.message_list().len(), 2);
}

/// The examples of docs/grpc.md that are request files or fragments load
/// too: the request file and the assertions list.
#[test]
fn the_grpc_page_examples_load() {
    let page = doc("grpc.md");
    let blocks = request_blocks(&page);
    assert_eq!(blocks.len(), 1);
    load(&blocks[0]);
    let start = page
        .find("assertions:\n  - grpc_status")
        .expect("the example");
    let block = &page[start..];
    let block = &block[..block.find("```").unwrap()];
    let file = format!(
        "version: 1\nkind: grpc\nname: r\ntarget: h:1\nmethod: a.S/M\nmessage: {{}}\n{block}"
    );
    let request = load(&file);
    assert_eq!(request.assertion_list().len(), 4);
    assert_eq!(request.captures.as_ref().unwrap().len(), 2);
}
