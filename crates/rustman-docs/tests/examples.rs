//! Opted-in examples, environment publishing rules and the leak scan
//! (api-docs-generation "Real examples are opt-in", "Redaction in
//! generated docs", "Environment values are not published by default").

mod common;

use common::{TOKEN, build_site, build_with, files, read, record, visible_text, workspace, write};
use rustman_docs::{ExampleSource, Format, MAX_EXAMPLE_BYTES};
use serde_json::json;

const ID: &str = "01JDQ3XG000000000000000001";

fn headers() -> serde_json::Value {
    json!([
        {"name": "Content-Type", "value": "application/json"},
        {"name": "Set-Cookie", "value": "sid=abc123456789"},
        {"name": "X-Api-Key", "value": "k-123456789"},
        {"name": "Date", "value": "Tue, 17 Nov 2026 09:30:00 GMT"}
    ])
}

fn opt_in(root: &std::path::Path) {
    write(
        root,
        "users/get.rman.yaml",
        &common::request(
            "Get user",
            "GET",
            "{{base_url}}/users/1",
            "docs:\n  example: true\n",
        ),
    );
}

#[test]
fn history_is_not_published_without_opting_in() {
    let ws = workspace("");
    record(
        ws.path(),
        ID,
        "users/get.rman.yaml",
        200,
        "{\"id\":1}",
        headers(),
    );
    let report = build_site(ws.path());
    assert!(report.published_examples.is_empty());
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    assert!(!page.contains("Example response"), "{page}");
}

#[test]
fn an_opted_in_request_shows_its_latest_successful_exchange() {
    let ws = workspace("");
    opt_in(ws.path());
    record(
        ws.path(),
        "01JDQ3XG000000000000000000",
        "users/get.rman.yaml",
        200,
        "{\"id\":0}",
        headers(),
    );
    let body = format!("{{\"id\":1,\"token\":\"{TOKEN}\",\"email\":\"ada@example.org\"}}");
    record(ws.path(), ID, "users/get.rman.yaml", 200, &body, headers());
    record(
        ws.path(),
        "01JDQ3XG000000000000000002",
        "users/get.rman.yaml",
        500,
        "{}",
        headers(),
    );
    let report = build_site(ws.path());
    assert_eq!(report.published_examples, ["users/get"]);
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    let text = visible_text(&page);
    assert!(
        text.contains("Example response Status 200 recorded 2026-11-17"),
        "{text}"
    );
    assert!(text.contains("Content-Type application/json"), "{text}");
    assert!(text.contains("Set-Cookie «redacted:Set-Cookie»"), "{text}");
    assert!(text.contains("X-Api-Key «redacted:X-Api-Key»"), "{text}");
    assert!(!text.contains("Date"), "noisy headers are left out: {text}");
    assert!(
        page.contains("&quot;token&quot;: &quot;«redacted:token»&quot;"),
        "{page}"
    );
    assert!(
        page.contains("&quot;id&quot;: 1,\n"),
        "pretty-printed: {page}"
    );
    assert!(
        !page.contains("abc123456789") && !page.contains("CANARY"),
        "{page}"
    );
}

#[test]
fn examples_listed_in_settings_and_redact_paths() {
    let ws = workspace("examples = [\"Get user\"]\nexample_redact_paths = [\"$.email\"]\n");
    record(
        ws.path(),
        ID,
        "users/get.rman.yaml",
        201,
        "{\"email\":\"ada@example.org\"}",
        json!([]),
    );
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    assert!(page.contains("«redacted:$.email»"), "{page}");
    assert!(!page.contains("ada@example.org"), "{page}");
}

#[test]
fn an_invalid_redact_path_is_a_usage_error() {
    let ws = workspace("example_redact_paths = [\"email\"]\n");
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string()
            .contains("[docs].example_redact_paths: 'email'"),
        "{err}"
    );
}

#[test]
fn long_example_bodies_are_truncated() {
    let ws = workspace("");
    opt_in(ws.path());
    let body = "x".repeat(MAX_EXAMPLE_BYTES + 100);
    record(ws.path(), ID, "users/get.rman.yaml", 200, &body, json!([]));
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    assert!(page.contains("Body truncated at 64 KB."), "truncation note");
    assert!(page.contains(&"x".repeat(MAX_EXAMPLE_BYTES)));
    assert!(!page.contains(&"x".repeat(MAX_EXAMPLE_BYTES + 1)));
}

#[test]
fn snapshot_examples_come_from_the_stored_snapshot() {
    let ws = workspace("");
    opt_in(ws.path());
    write(
        ws.path(),
        "users/__snapshots__/get.default.snap.yaml",
        &format!(
            "version: 1\nrequest: users/get.rman.yaml\nname: default\nkind: http\ncompare:\n  - status\n  - body\nstatus: 200\nbody:\n  json:\n    id: 7\n    secret: {TOKEN}\n"
        ),
    );
    record(
        ws.path(),
        ID,
        "users/get.rman.yaml",
        200,
        "{\"from\":\"history\"}",
        json!([]),
    );
    build_with(ws.path(), |o| o.examples = ExampleSource::Snapshot).unwrap();
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    let text = visible_text(&page);
    assert!(
        text.contains("Status 200 from the stored snapshot"),
        "{text}"
    );
    assert!(page.contains("&quot;id&quot;: 7"), "{page}");
    assert!(
        page.contains("«redacted:token»") && !page.contains("history"),
        "{page}"
    );
}

#[test]
fn variables_are_shown_by_name_only_by_default() {
    let ws = workspace("");
    build_site(ws.path());
    let site = ws.path().join("site");
    let page = visible_text(&read(&site, "endpoints/users-create.html"));
    assert!(page.contains("{{base_url}}/users"), "{page}");
    for (name, bytes) in files(&site) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains("localhost:8080"),
            "{name}"
        );
    }
}

#[test]
fn published_variables_need_env_and_never_secrets() {
    let ws = workspace("publish_variables = [\"base_url\"]\n");
    build_site(ws.path());
    assert!(!read(&ws.path().join("site"), "index.html").contains("localhost:8080"));
    build_with(ws.path(), |o| o.env = Some("local".into())).unwrap();
    let index = visible_text(&read(&ws.path().join("site"), "index.html"));
    assert!(
        index.contains("Variables Name Value base_url http://localhost:8080"),
        "{index}"
    );
    let err = build_with(ws.path(), |o| o.env = Some("prod".into())).unwrap_err();
    assert_eq!(err.exit_code(), 2);

    let ws = workspace("publish_variables = [\"token\"]\n");
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(err.to_string(), "token is a secret and cannot be published");
    assert_eq!(err.exit_code(), 2);
    assert!(!ws.path().join("site").exists(), "nothing is written");
}

#[test]
fn a_custom_template_printing_a_secret_fails_the_build() {
    let ws = workspace("template_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/endpoint.html",
        &format!("<p>{{{{ endpoint.name }}}} leaks {TOKEN}</p>\n"),
    );
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(err.exit_code(), 1);
    let message = err.to_string();
    assert!(
        message.starts_with("secret value found in site/endpoints/auth-list.html\n"),
        "{message}"
    );
    assert_eq!(message.lines().count(), 5, "{message}");
    let site = ws.path().join("site");
    let left = files(&site);
    assert!(
        !left.keys().any(|k| k.starts_with("endpoints/")),
        "{left:?}"
    );
    assert!(left.contains_key("index.html"), "clean files are kept");
    let manifest = read(&site, ".rustman-docs-manifest");
    assert!(!manifest.contains("endpoints/"), "{manifest}");
}

#[test]
fn a_custom_layout_template_overrides_the_builtin_one() {
    let ws = workspace("template_dirs = [\"templates\"]\n");
    write(
        ws.path(),
        "templates/endpoint.html",
        "<h1>Custom {{ endpoint.name }}</h1>\n",
    );
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/users-get.html");
    assert_eq!(page, "<h1>Custom Get user</h1>\n");
}

#[test]
fn markdown_example_bodies_are_tagged_json_only_when_they_are_json() {
    let ws = workspace("");
    opt_in(ws.path());
    record(
        ws.path(),
        ID,
        "users/get.rman.yaml",
        200,
        "{\"id\":7}",
        json!([]),
    );
    build_with(ws.path(), |o| o.format = Format::Markdown).unwrap();
    let page = read(&ws.path().join("site"), "endpoints/users-get.md");
    assert!(page.contains("```json\n{\n  \"id\": 7\n}\n```"), "{page}");

    record(
        ws.path(),
        ID,
        "users/get.rman.yaml",
        200,
        "plain text",
        json!([]),
    );
    build_with(ws.path(), |o| o.format = Format::Markdown).unwrap();
    let page = read(&ws.path().join("site"), "endpoints/users-get.md");
    assert!(page.contains("```\nplain text\n```"), "{page}");
}
