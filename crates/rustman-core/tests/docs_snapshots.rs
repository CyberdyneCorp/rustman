//! The examples of `docs/snapshots.md` (snapshot-testing task 1.4): every
//! request file loads, every snapshot file is read back and rendered again
//! byte for byte, and every response shown normalizes into the snapshot
//! that follows it.

use std::collections::HashMap;
use std::path::Path;

use rustman_core::Redactor;
use rustman_core::Secret;
use rustman_core::model::{Assertion, KindRegistry, Request, SnapshotAssertion};
use rustman_core::snapshot::{Exchange, Normalizer, SnapshotMeta, document, parse, to_yaml};

/// A fenced code block: its info string and its text.
struct Block {
    lang: String,
    text: String,
    /// The fence's indentation, removed from every line.
    indent: usize,
}

fn blocks(markdown: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut current: Option<Block> = None;
    for line in markdown.lines() {
        let fence = line.trim_start().strip_prefix("```");
        match (fence, current.take()) {
            (Some(_), Some(block)) => out.push(block),
            (Some(lang), None) => {
                current = Some(Block {
                    lang: lang.trim().to_owned(),
                    text: String::new(),
                    indent: line.len() - line.trim_start().len(),
                })
            }
            (None, Some(mut block)) => {
                block
                    .text
                    .push_str(line.get(block.indent..).unwrap_or_default());
                block.text.push('\n');
                current = Some(block);
            }
            (None, None) => {}
        }
    }
    out
}

/// A response written as `// response: <status>`, `// Name: value`
/// headers, then the body.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn response(text: &str) -> Option<Response> {
    let mut lines = text.lines();
    let status = lines.next()?.strip_prefix("// response: ")?.parse().ok()?;
    let mut headers = Vec::new();
    let mut body = Vec::new();
    for line in lines {
        match line.strip_prefix("// ").and_then(|h| h.split_once(": ")) {
            Some((name, value)) if body.is_empty() => headers.push((name.into(), value.into())),
            _ => body.push(line),
        }
    }
    Some(Response {
        status,
        headers,
        body: body.join("\n"),
    })
}

/// The `# path` comment on a block's first line, and the rest of the text.
fn named(block: &Block) -> Option<(&str, &str)> {
    let (first, rest) = block.text.split_once('\n')?;
    Some((first.strip_prefix("# ")?, rest))
}

fn snapshot_spec<'r>(request: &'r Request, name: &str) -> &'r SnapshotAssertion {
    let assertions = match request {
        Request::Http(http) => http.assertions.as_deref(),
        Request::Graphql(graphql) => graphql.assertions.as_deref(),
        Request::Mcp(mcp) => mcp.assertions.as_deref(),
        Request::Jsonrpc(_) | Request::Opaque(_) => None,
    };
    assertions
        .into_iter()
        .flatten()
        .find_map(|a| match a {
            Assertion::Snapshot(s) if s.name() == name => Some(&***s),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no snapshot named {name}"))
}

fn normalized_text(request_path: &str, spec: &SnapshotAssertion, response: &Response) -> String {
    let redactor = Redactor::new().with(&Secret::new("token", "abc123".to_owned()));
    let exchange = Exchange {
        kind: "http",
        status: Some(response.status),
        headers: &response.headers,
        body: response.body.as_bytes(),
        events: None,
    };
    let normalized = Normalizer::new(spec, &redactor)
        .unwrap()
        .normalize(&exchange);
    let compare = spec.compared();
    let meta = SnapshotMeta {
        request: request_path,
        name: spec.name(),
        kind: "http",
        compare: &compare,
    };
    to_yaml(&document(&meta, &normalized))
}

#[test]
fn every_example_loads_and_normalizes_as_shown() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/snapshots.md");
    let markdown = std::fs::read_to_string(doc_path).unwrap();
    let mut requests: HashMap<String, Request> = HashMap::new();
    let mut last_response: Option<Response> = None;
    let (mut loaded, mut snapshots, mut compared) = (0, 0, 0);
    for block in blocks(&markdown) {
        if let Some(r) = response(&block.text) {
            last_response = Some(r);
            continue;
        }
        if block.lang != "yaml" {
            continue;
        }
        let (path, text) = named(&block).expect("yaml examples start with `# <path>`");
        if path.ends_with(".rman.yaml") {
            let request = KindRegistry::default()
                .load(Path::new(path), text)
                .unwrap_or_else(|e| panic!("{e}"));
            requests.insert(path.to_owned(), request);
            loaded += 1;
            continue;
        }
        assert!(path.ends_with(".snap.yaml"), "unexpected example {path}");
        let doc = parse(Path::new(path), text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(to_yaml(&doc), text, "{path} is not in canonical form");
        snapshots += 1;
        let request_path = doc["request"].as_str().unwrap();
        let (Some(request), Some(response)) = (requests.get(request_path), last_response.take())
        else {
            continue;
        };
        let spec = snapshot_spec(request, doc["name"].as_str().unwrap());
        assert_eq!(
            normalized_text(request_path, spec, &response),
            text,
            "{path}"
        );
        compared += 1;
    }
    assert!(loaded >= 8, "{loaded} request examples");
    assert!(snapshots >= 8, "{snapshots} snapshot examples");
    assert!(compared >= 6, "{compared} normalized examples");
}
