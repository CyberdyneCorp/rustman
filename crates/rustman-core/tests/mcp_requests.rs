//! `kind: mcp` request files (add-mcp-client task 7.1): loading in a
//! workspace, interpolation of params, and the round trip load → save →
//! load, including a call saved from the TUI.

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::format::write::{mcp_to_yaml, request_to_yaml};
use rustman_core::model::{KindRegistry, McpOperation, McpRequest, Request};
use rustman_core::vars::{Resolver, SecretLookup, resolve_mcp};
use serde_json::{Map, Value, json};

const SEARCH: &str = "\
version: 1
kind: mcp
name: Search
description: Find files
tags: [mcp]
server: files
operation: tools/call
params:
  name: search
  arguments:
    q: rust
    token: '{{api_token}}'
timeout: 1500ms
validate: false
variables:
  page: 2
assertions:
- is_error: false
- content_text_contains: found
- structured: {path: $.count, gt: 0}
- output_schema: true
- latency: {total_lt: 2000}
captures:
- var: count
  from: json
  path: $.count
";

fn load(text: &str) -> McpRequest {
    match KindRegistry::default().load(Path::new("search.rman.yaml"), text) {
        Ok(Request::Mcp(mcp)) => *mcp,
        other => panic!("an mcp request: {other:?}"),
    }
}

#[test]
fn an_mcp_request_round_trips_through_save() {
    let loaded = load(SEARCH);
    assert_eq!(
        request_to_yaml(&Request::Mcp(Box::new(loaded.clone()))),
        SEARCH,
        "a loaded file is written back unchanged"
    );
    let written = mcp_to_yaml(&loaded);
    let again = load(&written);
    let mut expected = loaded.clone();
    expected.source = written.clone();
    assert_eq!(again, expected, "{written}");
    assert!(written.contains("operation: tools/call"), "{written}");
    assert!(written.contains("timeout: '1500ms'"), "{written}");
}

#[test]
fn a_saved_tool_call_is_a_kind_mcp_file() {
    let params: Map<String, Value> = json!({"name": "search", "arguments": {"q": "rust"}})
        .as_object()
        .cloned()
        .unwrap();
    let saved = McpRequest::new("Search rust", "files", McpOperation::ToolsCall, params);
    let text = request_to_yaml(&Request::Mcp(Box::new(saved.clone())));
    assert!(text.starts_with("version: 1\nkind: mcp\nname: Search rust\nserver: files\noperation: tools/call\nparams:\n"), "{text}");
    let again = load(&text);
    assert_eq!(again.server, "files");
    assert_eq!(again.operation, McpOperation::ToolsCall);
    assert_eq!(again.params(), saved.params());
    assert_eq!(
        load(&request_to_yaml(&Request::Mcp(Box::new(again.clone())))),
        again
    );
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| "s3cr3t-mcp".to_owned()))
    }
}

#[test]
fn params_are_interpolated_and_secrets_known() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("environments")).unwrap();
    fs::write(
        root.join("environments/prod.yaml"),
        "secrets: [api_token]\n",
    )
    .unwrap();
    fs::write(root.join("search.rman.yaml"), SEARCH).unwrap();
    let ws = Workspace::load(root).unwrap();
    let request = &ws.find_by_name("Search")[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    let resolved = resolve_mcp(request, &resolver).unwrap();
    assert_eq!(resolved.server, "files");
    assert_eq!(resolved.params["arguments"]["token"], "s3cr3t-mcp");
    assert_eq!(resolved.secrets.len(), 1);
    let shown = format!("{resolved:?}");
    assert!(!shown.contains("s3cr3t-mcp"), "{shown}");
}
