//! Protocol reference pages (api-docs-generation "GraphQL and JSON-RPC
//! reference pages", "Other request kinds"): a schema reference from the
//! GraphQL schema cache or `schema_file`, method pages from OpenRPC
//! documents, both omitted when absent, and generic pages for every other
//! kind.

mod common;

use std::path::Path;

use common::{build_with, read, visible_text, workspace, write};
use rustman_core::Redactor;
use rustman_docs::Format;
use rustman_graphql::{GraphqlSchema, SchemaCache};
use serde_json::{Value, json};

const LEGACY: &str = include_str!("../../rustman-graphql/tests/fixtures/legacy.introspection.json");
const LIBRARY: &str = include_str!("../../rustman-graphql/tests/fixtures/library.graphql");

fn build_both(root: &Path) -> rustman_docs::BuildReport {
    build_with(root, |o| o.format = Format::Both).unwrap()
}

fn graphql_request(root: &Path, extra: &str) {
    write(
        root,
        "api/user.rman.yaml",
        &format!(
            "version: 1\nkind: graphql\nname: User\nendpoint: '{{{{base_url}}}}/graphql'\ndocument: 'query {{ user(id: 1) {{ email }} }}'\n{extra}"
        ),
    );
}

/// Cache the legacy schema for `http://localhost:8080/graphql`, the
/// endpoint the `local` environment resolves to.
fn cache_schema(root: &Path) {
    let result: Value = serde_json::from_str(LEGACY).unwrap();
    let schema = GraphqlSchema::from_introspection(&result, "test").unwrap();
    SchemaCache::for_workspace(root)
        .store(
            "http://localhost:8080/graphql",
            &result,
            schema,
            &Redactor::new(),
        )
        .unwrap();
}

fn reference_files(report: &rustman_docs::BuildReport) -> Vec<&str> {
    report
        .files
        .iter()
        .filter(|f| f.starts_with("reference/"))
        .map(String::as_str)
        .collect()
}

#[test]
fn a_cached_graphql_schema_gets_a_page_per_type() {
    let ws = workspace("");
    graphql_request(ws.path(), "");
    cache_schema(ws.path());
    let report = build_both(ws.path());
    assert_eq!(
        reference_files(&report),
        [
            "reference/graphql-schema-query.html",
            "reference/graphql-schema-query.md",
            "reference/graphql-schema-role.html",
            "reference/graphql-schema-role.md",
            "reference/graphql-schema-subscription.html",
            "reference/graphql-schema-subscription.md",
            "reference/graphql-schema-user.html",
            "reference/graphql-schema-user.md",
        ]
    );
    let site = ws.path().join("site");
    let user = visible_text(&read(&site, "reference/graphql-schema-user.html"));
    assert!(
        user.contains("Field Type Arguments Deprecated Description email String"),
        "{user}"
    );
    assert!(user.contains("username String Use handle"), "{user}");
    let md = read(&site, "reference/graphql-schema-user.md");
    assert!(md.contains("| email | String |  |  |  |"), "{md}");
    assert!(
        md.contains("| username | String |  | Use handle |  |"),
        "{md}"
    );
    let index = visible_text(&read(&site, "index.html"));
    assert!(
        index.contains("GraphQL schema {{base_url}}/graphql"),
        "{index}"
    );
    let readme = read(&site, "README.md");
    assert!(
        readme.contains("- [User](reference/graphql-schema-user.md) *object*"),
        "{readme}"
    );
}

#[test]
fn deprecations_and_arguments_are_listed() {
    let ws = workspace("");
    write(ws.path(), "schema/library.graphql", LIBRARY);
    graphql_request(ws.path(), "schema_file: ../schema/library.graphql\n");
    build_both(ws.path());
    let site = ws.path().join("site");
    insta::assert_snapshot!(
        "graphql_query_md",
        read(&site, "reference/graphql-schema-query.md")
    );
    let readme = read(&site, "README.md");
    let at = readme.find("## GraphQL schema").unwrap();
    insta::assert_snapshot!("graphql_readme", &readme[at..]);
}

#[test]
fn no_graphql_section_without_graphql_requests_or_schema() {
    let ws = workspace("");
    cache_schema(ws.path());
    let report = build_both(ws.path());
    assert!(reference_files(&report).is_empty());
    assert!(!read(&ws.path().join("site"), "index.html").contains("GraphQL schema"));
    let other = workspace("");
    graphql_request(other.path(), "");
    let report = build_both(other.path());
    assert!(reference_files(&report).is_empty(), "no cached schema");
    assert!(!read(&other.path().join("site"), "README.md").contains("GraphQL schema"));
}

fn openrpc() -> Value {
    json!({
        "openrpc": "1.2.6",
        "info": {"title": "node", "version": "1"},
        "methods": [
            {
                "name": "eth_getBalance",
                "summary": "Balance of an <account>.",
                "params": [
                    {"name": "address", "required": true, "schema": {"type": "string"}},
                    {"name": "block", "schema": {"type": "string"}}
                ],
                "result": {"name": "balance", "schema": {"type": "string", "pattern": "^0x"}},
                "errors": [{"code": -32000, "message": "header not found"}]
            },
            {"name": "eth_blockNumber", "deprecated": true, "params": []}
        ]
    })
}

fn jsonrpc_request(root: &Path) {
    write(
        root,
        "rpc/balance.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Balance\nendpoint: '{{base_url}}/rpc'\nmethod: eth_getBalance\nparams: ['0x0', latest]\n",
    );
}

#[test]
fn openrpc_catalogs_get_method_pages() {
    let ws = workspace("\n[jsonrpc.catalogs]\n\"{{base_url}}/rpc\" = \"openrpc.json\"\n");
    jsonrpc_request(ws.path());
    write(ws.path(), "openrpc.json", &openrpc().to_string());
    let report = build_both(ws.path());
    assert_eq!(
        reference_files(&report),
        [
            "reference/json-rpc-methods-eth-blocknumber.html",
            "reference/json-rpc-methods-eth-blocknumber.md",
            "reference/json-rpc-methods-eth-getbalance.html",
            "reference/json-rpc-methods-eth-getbalance.md",
        ]
    );
    let site = ws.path().join("site");
    let html = read(&site, "reference/json-rpc-methods-eth-getbalance.html");
    assert!(html.contains("Balance of an &lt;account&gt;."), "{html}");
    let text = visible_text(&html);
    assert!(
        text.contains("Params Name Required Schema address yes"),
        "{text}"
    );
    assert!(
        text.contains("Errors Code Message -32000 header not found"),
        "{text}"
    );
    assert!(text.contains("Schemas Result {"), "{text}");
    let md = read(&site, "reference/json-rpc-methods-eth-getbalance.md");
    assert_eq!(html_h2(&html), md_h2(&md));
    let deprecated = read(&site, "reference/json-rpc-methods-eth-blocknumber.md");
    assert!(deprecated.contains("**deprecated**"), "{deprecated}");
    insta::assert_snapshot!(
        "jsonrpc_method_md",
        read(&site, "reference/json-rpc-methods-eth-getbalance.md")
    );
}

#[test]
fn a_discovered_openrpc_document_is_read_from_the_cache() {
    let ws = workspace("");
    jsonrpc_request(ws.path());
    rustman_jsonrpc::catalog::write_cache(
        ws.path(),
        "http://localhost:8080/rpc",
        openrpc(),
        &Redactor::new(),
    )
    .unwrap();
    let report = build_both(ws.path());
    assert_eq!(reference_files(&report).len(), 4);
}

#[test]
fn no_method_pages_without_a_catalog() {
    let ws = workspace("");
    jsonrpc_request(ws.path());
    let report = build_both(ws.path());
    assert!(reference_files(&report).is_empty());
    assert!(!read(&ws.path().join("site"), "index.html").contains("JSON-RPC methods"));
}

#[test]
fn hidden_requests_contribute_no_reference() {
    let ws = workspace("");
    graphql_request(ws.path(), "docs:\n  hidden: true\n");
    cache_schema(ws.path());
    let report = build_both(ws.path());
    assert!(reference_files(&report).is_empty());
}

#[test]
fn mcp_and_unregistered_kinds_get_generic_pages() {
    let ws = workspace("");
    write(
        ws.path(),
        "tools/search.rman.yaml",
        "version: 1\nkind: mcp\nname: Search\nserver: files\noperation: tools/call\nparams:\n  name: search\n  arguments:\n    q: rust\n",
    );
    write(
        ws.path(),
        "events/orders.rman.yaml",
        "version: 1\nkind: nats\nname: Orders\ntopic: orders\nheaders:\n  - name: Authorization\n    value: Bearer literal-kafka-cred\n",
    );
    let workspace = rustman_docs::load_workspace(ws.path(), Some(ws.path())).unwrap();
    let mut options = rustman_docs::BuildOptions::new(ws.path().join("site"));
    options.format = Format::Both;
    rustman_docs::build(&workspace, &common::secrets(), &options).unwrap();
    let site = ws.path().join("site");
    let mcp = visible_text(&read(&site, "endpoints/tools-search.html"));
    assert!(mcp.contains("kind: mcp"), "{mcp}");
    assert!(mcp.contains("server files"), "{mcp}");
    assert!(mcp.contains("operation tools/call"), "{mcp}");
    let kafka = read(&site, "endpoints/events-orders.md");
    assert!(kafka.contains("Kind: `nats`"), "{kafka}");
    assert!(kafka.contains("| `topic` | `orders` |"), "{kafka}");
    assert!(!kafka.contains("literal-kafka-cred"), "{kafka}");
    assert!(!kafka.contains("Code samples"), "{kafka}");
}

/// Review regression: the Markdown and HTML outputs of every page but the
/// index have the same `##`/`<h2>` and `###`/`<h3>` headings (a JSON-RPC
/// method page had `## Schemas` in Markdown but `<h2>Result</h2>` in HTML).
#[test]
fn every_page_has_the_same_headings_in_markdown_and_html() {
    let ws = workspace("\n[jsonrpc.catalogs]\n\"{{base_url}}/rpc\" = \"openrpc.json\"\n");
    jsonrpc_request(ws.path());
    write(ws.path(), "openrpc.json", &openrpc().to_string());
    graphql_request(ws.path(), "variables:\n  id: '{{user_id}}'\n");
    cache_schema(ws.path());
    write(
        ws.path(),
        "tools/search.rman.yaml",
        "version: 1\nkind: mcp\nname: Search\nserver: files\noperation: tools/call\nparams:\n  name: search\n",
    );
    let report = build_both(ws.path());
    let site = ws.path().join("site");
    let mut compared = 0;
    // The index differs by design: README.md lists the sections as one
    // nested list, index.html has a heading per section.
    let pages = report.files.iter().filter(|f| f.ends_with(".html"));
    for html_file in pages.filter(|f| *f != "index.html") {
        let md_file = html_file.replace(".html", ".md");
        let (html, md) = (read(&site, html_file), read(&site, &md_file));
        for level in [2, 3] {
            assert_eq!(
                html_headings(&html, level),
                md_headings(&md, level),
                "h{level} of {html_file} vs {md_file}"
            );
        }
        compared += 1;
    }
    assert!(compared >= 10, "{compared} pages compared");
}

fn html_h2(html: &str) -> Vec<String> {
    html_headings(html, 2)
}

fn md_h2(md: &str) -> Vec<String> {
    md_headings(md, 2)
}

/// The text of every `<hN>` heading, tags stripped and entities decoded.
fn html_headings(html: &str, level: usize) -> Vec<String> {
    let (open, close) = (format!("<h{level}"), format!("</h{level}>"));
    html.split(&open)
        .skip(1)
        .filter_map(|rest| rest.split_once('>').map(|(_, r)| r))
        .filter_map(|rest| rest.split_once(&close).map(|(h, _)| h))
        .map(|h| unescape(&strip_tags(h)))
        .collect()
}

/// The text of every ATX heading of `level`, outside fenced code blocks,
/// with backslash escapes and code spans removed. The per-language
/// headings under `## Code samples` are tabs in HTML, so they are skipped.
fn md_headings(md: &str, level: usize) -> Vec<String> {
    let prefix = format!("{} ", "#".repeat(level));
    let (mut fence, mut samples) = (false, false);
    let mut out = Vec::new();
    for line in md.lines() {
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence && line.starts_with("## ") {
            samples = line == "## Code samples";
        }
        if fence || (samples && level > 2) {
            continue;
        }
        if let Some(text) = line.strip_prefix(&prefix) {
            let text = strip_tags(text).replace(['\\', '`'], "");
            out.push(unescape(text.trim()));
        }
    }
    out
}

fn strip_tags(text: &str) -> String {
    visible_text(text)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}
