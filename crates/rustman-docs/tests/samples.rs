//! Code samples (api-docs-generation "Code samples"): the snippet engine in
//! placeholder mode, `[docs].samples` selection and order, and custom
//! templates from `[docs].template_dirs` with their context version.

mod common;

use common::{TOKEN, build_with, read, request, visible_text, workspace, write};
use rustman_docs::Format;

/// The `## Code samples` part of an endpoint's Markdown page.
fn samples_of(root: &std::path::Path, slug: &str) -> String {
    let page = read(&root.join("site"), &format!("endpoints/{slug}.md"));
    let at = page
        .find("## Code samples")
        .unwrap_or_else(|| panic!("{page}"));
    page[at..].to_owned()
}

fn build_markdown(root: &std::path::Path) -> rustman_docs::BuildReport {
    build_with(root, |o| o.format = Format::Both).unwrap()
}

/// A workspace with one request per body type.
fn bodies() -> tempfile::TempDir {
    let ws = workspace("");
    let root = ws.path();
    write(
        root,
        "bodies/form.rman.yaml",
        &request(
            "Form",
            "POST",
            "{{base_url}}/form",
            "body:\n  type: form\n  fields:\n    - name: q\n      value: a b&c\n    - name: page\n      value: '{{page}}'\n",
        ),
    );
    write(
        root,
        "bodies/multipart.rman.yaml",
        &request(
            "Upload",
            "POST",
            "{{base_url}}/upload",
            "body:\n  type: multipart\n  parts:\n    - name: title\n      value: Report\n    - name: file\n      file: data/report.csv\n      content_type: text/csv\n",
        ),
    );
    write(
        root,
        "bodies/file.rman.yaml",
        &request(
            "Raw file",
            "PUT",
            "{{base_url}}/raw",
            "headers:\n  - name: Content-Type\n    value: application/octet-stream\nbody:\n  type: file\n  path: data/report.csv\n",
        ),
    );
    write(root, "data/report.csv", "a,b\n1,2\n");
    ws
}

#[test]
fn default_samples_are_curl_httpie_fetch_python_rust_in_order() {
    let ws = workspace("");
    build_markdown(ws.path());
    let html = read(&ws.path().join("site"), "endpoints/users-create.html");
    let text = visible_text(&html);
    assert!(
        text.contains("Code samples curl httpie fetch python rust"),
        "{text}"
    );
    let md = samples_of(ws.path(), "users-create");
    let headings: Vec<&str> = md.lines().filter(|l| l.starts_with("### ")).collect();
    assert_eq!(
        headings,
        [
            "### curl",
            "### httpie",
            "### fetch",
            "### python",
            "### rust"
        ]
    );
}

#[test]
fn samples_keep_variables_and_redact_secrets() {
    let ws = workspace("");
    build_markdown(ws.path());
    let md = samples_of(ws.path(), "users-create");
    assert!(md.contains("curl '{{base_url}}/users'"), "{md}");
    assert!(md.contains("BASE_URL = \"{{base_url}}\""), "{md}");
    assert!(md.contains("«redacted:token»"), "{md}");
    assert!(!md.contains("localhost:8080"), "no environment value: {md}");
    assert!(!md.contains(TOKEN), "{md}");
}

#[test]
fn dynamic_values_are_kept_and_the_process_environment_is_never_read() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/trace.rman.yaml",
        &request(
            "Trace",
            "GET",
            "{{base_url}}/trace/{{$uuid}}",
            "headers:\n  - name: X-Home\n    value: '{{$env PATH}}'\n",
        ),
    );
    build_markdown(ws.path());
    let md = samples_of(ws.path(), "users-trace");
    assert!(md.contains("/trace/{{$uuid}}"), "{md}");
    assert!(md.contains("{{$env PATH}}"), "{md}");
    let path = std::env::var("PATH").unwrap_or_default();
    assert!(path.is_empty() || !md.contains(&path), "{md}");
}

#[test]
fn snapshots_per_language_and_body_type() {
    let ws = bodies();
    build_markdown(ws.path());
    for slug in [
        "users-create",
        "bodies-form",
        "bodies-multipart",
        "bodies-file",
    ] {
        insta::assert_snapshot!(format!("samples_{slug}"), samples_of(ws.path(), slug));
    }
}

#[test]
fn graphql_and_jsonrpc_requests_get_their_http_samples() {
    let ws = workspace("");
    write(
        ws.path(),
        "api/users.rman.yaml",
        "version: 1\nkind: graphql\nname: Users\nendpoint: '{{base_url}}/graphql'\ndocument: 'query { users { id } }'\n",
    );
    write(
        ws.path(),
        "api/block.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: Block\nendpoint: '{{base_url}}/rpc'\nmethod: eth_blockNumber\n",
    );
    write(
        ws.path(),
        "api/tool.rman.yaml",
        "version: 1\nkind: mcp\nname: Tool\nserver: files\noperation: tools/list\n",
    );
    build_markdown(ws.path());
    let graphql = samples_of(ws.path(), "api-users");
    assert!(graphql.contains("'{{base_url}}/graphql'"), "{graphql}");
    assert!(graphql.contains("users { id }"), "{graphql}");
    let jsonrpc = samples_of(ws.path(), "api-block");
    assert!(jsonrpc.contains("eth_blockNumber"), "{jsonrpc}");
    let mcp = read(&ws.path().join("site"), "endpoints/api-tool.md");
    assert!(!mcp.contains("Code samples"), "{mcp}");
}

#[test]
fn samples_select_and_order_languages_and_custom_templates_replace_them() {
    let ws = workspace("samples = [\"curl\", \"go\"]\ntemplate_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/go.tmpl",
        "{# docs_context_version = 1 #}\n// custom v{{ docs_context_version }}\nhttp.NewRequest({{ method|q }}, {{ url|q }}, nil)\n",
    );
    let report = build_markdown(ws.path());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let md = samples_of(ws.path(), "users-create");
    let headings: Vec<&str> = md.lines().filter(|l| l.starts_with("### ")).collect();
    assert_eq!(headings, ["### curl", "### go"]);
    assert!(
        md.contains("// custom v1\nhttp.NewRequest(\"POST\", BASE_URL + \"/users\", nil)"),
        "{md}"
    );
    let html = visible_text(&read(&ws.path().join("site"), "endpoints/users-get.html"));
    assert!(html.contains("Code samples curl go"), "{html}");
}

#[test]
fn a_template_for_another_context_version_falls_back_with_a_warning() {
    let ws = workspace("samples = [\"go\"]\ntemplate_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/go.tmpl",
        "{# docs_context_version = 0 #}\ncustom\n",
    );
    let report = build_markdown(ws.path());
    assert_eq!(
        report.warnings,
        [
            "docs: template templates/go.tmpl targets docs_context_version 0, this rustman provides 1; using the built-in go template"
        ]
    );
    let md = samples_of(ws.path(), "users-create");
    assert!(!md.contains("custom"), "{md}");
    assert!(md.contains("package main"), "{md}");
}

#[test]
fn a_broken_custom_template_fails_the_build() {
    let ws = workspace("samples = [\"python\"]\ntemplate_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/python.tmpl",
        "{# docs_context_version = 1 #}\n{{ no_such_value }}\n",
    );
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string()
            .starts_with("docs: template templates/python.tmpl: "),
        "{err}"
    );
}

#[test]
fn an_unknown_sample_language_is_a_usage_error() {
    let ws = workspace("samples = [\"cobol\"]\n");
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string()
            .starts_with("docs: [docs].samples: unknown language 'cobol'"),
        "{err}"
    );
}

#[test]
fn a_custom_template_printing_a_secret_is_redacted() {
    let ws = workspace("samples = [\"curl\"]\ntemplate_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/curl.tmpl",
        &format!("{{# docs_context_version = 1 #}}\n{TOKEN}\n"),
    );
    build_markdown(ws.path());
    assert!(samples_of(ws.path(), "users-create").contains("«redacted:token»"));
    for (name, bytes) in common::files(&ws.path().join("site")) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TOKEN),
            "{name} kept the secret"
        );
    }
}
