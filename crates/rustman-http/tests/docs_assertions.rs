//! Every example in docs/assertions.md is a passing fixture (task 5.5).

mod support;

use std::borrow::Cow;
use std::fs;
use std::path::Path;

use rustman_core::assertions::{self, Context, ResponseView};
use rustman_core::model::{Assertion, Checked, SchemaAssertion};
use rustman_core::vars::Resolver;
use rustman_core::{Redactor, SecretStore};
use support::TestServer;
use support::collection::{run, write};
use support::routes::TOKEN;

/// A fenced block of the page: its language, the line before it and its lines.
struct Block {
    lang: String,
    before: String,
    lines: Vec<String>,
}

fn blocks() -> Vec<Block> {
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/assertions.md");
    let text = fs::read_to_string(doc).unwrap().replace("\r\n", "\n");
    let mut out = Vec::new();
    let mut before = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(lang) = line.strip_prefix("```") {
            let body: Vec<String> = lines
                .by_ref()
                .take_while(|l| *l != "```")
                .map(str::to_owned)
                .collect();
            out.push(Block {
                lang: lang.to_owned(),
                before: before.clone(),
                lines: body,
            });
        } else if !line.trim().is_empty() {
            before = line.to_owned();
        }
    }
    out
}

/// Write every file the page shows into `root`, pointing `base_url` at `server`.
fn write_examples(root: &Path, server: &TestServer) -> usize {
    let mut files = 0;
    for block in blocks() {
        let (path, body) = match block.lang.as_str() {
            "yaml" => {
                let path = block.lines[0]
                    .strip_prefix("# ")
                    .expect("starts with '# <path>'");
                (path.to_owned(), block.lines[1..].join("\n") + "\n")
            }
            "json" => {
                let path = block.before.split('`').nth(1).expect("File `<path>`:");
                (path.to_owned(), block.lines.join("\n") + "\n")
            }
            _ => continue,
        };
        let body = if path.starts_with("environments/") {
            body.replace("http://localhost:8080", &server.url(""))
        } else {
            body
        };
        write(root, &[(path.as_str(), body.as_str())]);
        files += 1;
    }
    files
}

#[tokio::test]
async fn every_example_passes() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        &[
            (
                "rustman.toml",
                "[workspace]\nname = \"docs\"\nversion = 1\n",
            ),
            (".gitignore", ".rustman/\n"),
        ],
    );
    assert_eq!(write_examples(root, &server), 7);

    let ran = run(root, "", "local").await;
    let names: Vec<&str> = ran.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Log in",
            "Who am I",
            "Create order",
            "Get user",
            "User matches its schema"
        ]
    );
    for r in &ran {
        assert!(r.passed(), "{}: {:#?}", r.name, r.assertions);
        assert!(
            r.captures.warnings.is_empty(),
            "{}: {:?}",
            r.name,
            r.captures
        );
        assert!(!r.assertions.is_empty());
    }

    // `persist: true` wrote user_id to the state file; the secret token went
    // to the secret store instead.
    let state = fs::read_to_string(root.join(".rustman/state/local.yaml")).unwrap();
    assert_eq!(state, "user_id: '42'\n");
    let store = SecretStore::with_keychain(root, None);
    assert_eq!(
        store.get("local", "token").unwrap().unwrap().expose(),
        TOKEN
    );
}

/// The schema failure shown on the page is the message Rustman produces for
/// `schemas/user.json` and a body with `"email": 5` and no `id`.
#[test]
fn the_schema_failure_example_is_exact() {
    let block = blocks()
        .into_iter()
        .find(|b| b.lang == "text" && b.lines[0].starts_with("schema:"))
        .unwrap();
    let schema_file = blocks()
        .into_iter()
        .find(|b| b.lang == "json")
        .unwrap()
        .lines
        .join("\n");
    let schema = SchemaAssertion {
        file: None,
        inline: Some(serde_json::from_str(&schema_file).unwrap()),
    };
    let list = [Assertion::Schema(Checked::new(schema).unwrap())];
    let response = ResponseView {
        status: 200,
        body: Cow::Borrowed(br#"{"email":5}"#),
        ..ResponseView::default()
    };
    let resolver = Resolver::new();
    let redactor = Redactor::new();
    let ctx = Context {
        resolver: &resolver,
        request: None,
        redactor: &redactor,
    };
    let results = assertions::evaluate(&list, &response, &ctx);
    assert_eq!(results[0].message, block.lines[0]);
}
