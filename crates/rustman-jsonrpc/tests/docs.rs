//! The `kind: jsonrpc` examples in docs/request-files.md and
//! docs/jsonrpc.md load (add-jsonrpc-client task 1.3).

use std::path::Path;

use rustman_core::model::{CallFields, JsonrpcRequest, KindRegistry, Request};
use serde_json::json;

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// Every fenced `yaml` block of `text` that is a request file.
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
            if text.contains("kind: jsonrpc") {
                out.push(text);
            }
        }
    }
    out
}

/// The JSON-RPC section of request-files.md.
fn section() -> String {
    let text = doc("request-files.md");
    let start = text.find("## JSON-RPC requests").expect("the section");
    let rest = &text[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    rest[..end].to_owned()
}

fn load(text: &str) -> JsonrpcRequest {
    let request = KindRegistry::default()
        .load(Path::new("example.rman.yaml"), text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
    let Request::Jsonrpc(jsonrpc) = request else {
        panic!("a jsonrpc request");
    };
    *jsonrpc
}

fn methods(request: &JsonrpcRequest) -> Vec<&str> {
    request
        .calls()
        .iter()
        .map(|c: &CallFields<'_>| c.method.unwrap_or_default())
        .collect()
}

#[test]
fn the_request_file_examples_load() {
    let blocks = request_blocks(&section());
    assert_eq!(blocks.len(), 4);
    let eth = load(&blocks[0]);
    assert_eq!(methods(&eth), ["eth_blockNumber"]);
    assert!(eth.is_idempotent());
    assert_eq!(eth.retry().effective_attempts(), 2);
    let btc = load(&blocks[1]);
    assert_eq!(
        methods(&btc),
        ["getblockcount", "getblockhash", "getblockchaininfo"]
    );
    assert_eq!(btc.calls()[1].params.unwrap().get(), &json!([800000]));
    let solana = load(&blocks[2]);
    assert_eq!(
        solana.params.as_ref().unwrap().get(),
        &json!(["{{wallet}}", {"commitment": "finalized"}])
    );
    let heads = load(&blocks[3]);
    assert_eq!(methods(&heads), ["eth_subscribe"]);
    assert_eq!(heads.subscription.unwrap().effective_max_events(), 3);
}

#[test]
fn the_jsonrpc_page_examples_load() {
    let blocks = request_blocks(&doc("jsonrpc.md"));
    assert_eq!(blocks.len(), 2);
    for block in &blocks {
        load(block);
    }
}

#[test]
fn quoted_hex_stays_a_string() {
    let text =
        "version: 1\nkind: jsonrpc\nname: h\nendpoint: http://n\nmethod: m\nparams: ['0x10']\n";
    assert_eq!(load(text).params.unwrap().get(), &json!(["0x10"]));
}
