//! Deterministic output (api-docs-generation "Deterministic output",
//! design D5): a golden test builds a rich fixture twice in both formats
//! and compares every file; the hash of each file is a snapshot, so CI on
//! Linux, macOS and Windows proves the bytes are the same everywhere
//! (sorting, no timestamps, `\n` line endings, UTF-8 without a BOM,
//! stable anchors and content-hashed assets).

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{build_with, files, record, request, workspace, write};
use rustman_core::snapshot::sha256_hex;
use rustman_docs::Format;
use serde_json::json;

const LIBRARY: &str = include_str!("../../rustman-graphql/tests/fixtures/library.graphql");

/// The common fixture plus every page kind: nested folders, slug
/// collisions, form and multipart bodies, a recorded example, a GraphQL
/// schema, an OpenRPC catalog and a gRPC request.
fn rich_workspace() -> tempfile::TempDir {
    let ws = workspace(
        "examples = [\"users/get\"]\n\n[jsonrpc.catalogs]\n\"{{base_url}}/rpc\" = \"openrpc.json\"\n",
    );
    let root = ws.path();
    write(
        root,
        "users/admin/promote.rman.yaml",
        &request(
            "Promote",
            "POST",
            "{{base_url}}/admin/promote",
            "body:\n  type: form\n  fields:\n    - name: role\n      value: admin\n",
        ),
    );
    write(
        root,
        "users/admin-promote.rman.yaml",
        &request(
            "Promote (legacy)",
            "POST",
            "{{base_url}}/promote",
            "body:\n  type: multipart\n  parts:\n    - name: note\n      value: hi\n",
        ),
    );
    write(root, "schema/library.graphql", LIBRARY);
    write(
        root,
        "graph/users.rman.yaml",
        "version: 1\nkind: graphql\nname: Users\nendpoint: '{{base_url}}/graphql'\nschema_file: ../schema/library.graphql\ndocument: 'query { users { id } }'\n",
    );
    write(
        root,
        "openrpc.json",
        &json!({"methods": [{"name": "eth_chainId", "params": [], "result": {"name": "id", "schema": {"type": "string"}}}]})
            .to_string(),
    );
    write(
        root,
        "rpc/chain.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Chain\nendpoint: '{{base_url}}/rpc'\nmethod: eth_chainId\n",
    );
    write(
        root,
        "rpc/product.rman.yaml",
        "version: 1\nkind: grpc\nname: GetProduct\ntarget: localhost:50051\nmethod: shop.Catalog/GetProduct\nmessage:\n  id: 7\n",
    );
    record(
        root,
        "01JD0000000000000000000001",
        "users/get.rman.yaml",
        200,
        "{\"id\":1,\"name\":\"Ada\"}",
        json!([["content-type", "application/json"]]),
    );
    ws
}

fn build_into(root: &Path, out: &str) -> BTreeMap<String, Vec<u8>> {
    build_with(root, |o| {
        o.out = root.join(out);
        o.format = Format::Both;
    })
    .unwrap();
    files(&root.join(out))
}

#[test]
fn two_builds_of_both_formats_are_byte_identical() {
    let ws = rich_workspace();
    let a = build_into(ws.path(), "a");
    let b = build_into(ws.path(), "b");
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (name, bytes) in &a {
        assert!(b[name] == *bytes, "{name} differs between builds");
        assert!(!bytes.contains(&b'\r'), "{name} has CR");
        assert!(!bytes.starts_with(b"\xEF\xBB\xBF"), "{name} has a BOM");
    }
    for kind in ["reference/", "endpoints/rpc-product", "assets/app."] {
        assert!(a.keys().any(|n| n.starts_with(kind)), "{kind} missing");
    }
}

#[test]
fn rebuilding_in_place_changes_nothing() {
    let ws = rich_workspace();
    let first = build_into(ws.path(), "site");
    let second = build_into(ws.path(), "site");
    assert_eq!(first, second);
}

#[test]
fn no_build_timestamp_is_written() {
    let ws = rich_workspace();
    let today = &rustman_core::run::now_rfc3339()[..10];
    for (name, bytes) in build_into(ws.path(), "site") {
        let text = String::from_utf8(bytes).unwrap_or_else(|_| panic!("{name} is not UTF-8"));
        assert!(!text.contains(today), "{name} holds today's date");
    }
}

/// The SHA-256 (first 16 hex digits) of every file, with the version
/// written as `<version>` first, so the golden survives releases.
#[test]
fn golden_hashes_are_the_same_on_every_platform() {
    let ws = rich_workspace();
    let site = build_into(ws.path(), "site");
    let version = rustman_core::VERSION.as_bytes();
    let lines: Vec<String> = site
        .iter()
        .map(|(name, bytes)| {
            let normalized = replace(bytes, version, b"<version>");
            format!("{}  {name}", &sha256_hex(&normalized)[..16])
        })
        .collect();
    insta::assert_snapshot!("golden_hashes", lines.join("\n"));
}

fn replace(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}
