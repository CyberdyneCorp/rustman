//! The `kind: ws` and `kind: sse` examples in docs/request-files.md and
//! docs/realtime.md load, with the fields the docs describe
//! (add-realtime-client task 4.5).

use std::path::Path;

use rustman_core::model::{Assertion, KindRegistry, Request, SseRequest, Step, WsRequest};

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// Every fenced `yaml` block of `text` that is a realtime request file.
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
            if text.contains("kind: ws\n") || text.contains("kind: sse\n") {
                out.push(text);
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

fn ws(text: &str) -> WsRequest {
    match load(text) {
        Request::Ws(ws) => *ws,
        other => panic!("a ws request, got {}", other.kind()),
    }
}

fn sse(text: &str) -> SseRequest {
    match load(text) {
        Request::Sse(sse) => *sse,
        other => panic!("an sse request, got {}", other.kind()),
    }
}

#[test]
fn every_example_loads() {
    let mut count = 0;
    for name in ["request-files.md", "realtime.md"] {
        for block in request_blocks(&doc(name)) {
            load(&block);
            count += 1;
        }
    }
    assert_eq!(count, 5, "the docs have five realtime examples");
}

#[test]
fn the_request_files_examples_read_as_documented() {
    let blocks = request_blocks(&doc("request-files.md"));
    let ticker = ws(&blocks[0]);
    assert_eq!(
        ticker.subprotocols.as_deref(),
        Some(&["v2.feed".to_owned(), "v1.feed".to_owned()][..])
    );
    let limits = ticker.effective_limits();
    assert_eq!(limits.effective_max_messages(), 20);
    assert_eq!(limits.effective_max_message_bytes(), 16 * 1024 * 1024);
    let assertions = ticker.assertions.as_ref().unwrap();
    assert!(matches!(assertions[0].get(), Assertion::Messages(_)));
    let ping = ws(&blocks[1]);
    let steps = ping.step_list();
    assert_eq!(steps.len(), 5);
    assert!(matches!(steps[1], Step::WaitFor(_)));
    assert!(matches!(steps[4], Step::Close(_)));
    let assertions = ping.assertions.as_ref().unwrap();
    assert_eq!(*assertions[0].get(), Assertion::CloseCode(1000));
    let chat = sse(&blocks[2]);
    assert_eq!(chat.effective_method(), "POST");
    assert_eq!(chat.effective_reconnect().effective_max_attempts(), 2);
}

#[test]
fn the_realtime_page_examples_read_as_documented() {
    let blocks = request_blocks(&doc("realtime.md"));
    let orders = ws(&blocks[0]);
    assert_eq!(orders.step_list().len(), 5);
    assert_eq!(orders.effective_limits().effective_max_messages(), 50);
    let prices = sse(&blocks[1]);
    assert_eq!(prices.effective_method(), "GET");
    assert_eq!(prices.effective_limits().effective_max_messages(), 5);
    assert_eq!(prices.captures.as_ref().unwrap().len(), 1);
}
