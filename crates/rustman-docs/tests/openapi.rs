//! OpenAPI enrichment from the import snapshot (api-docs-generation
//! "OpenAPI enrichment").

mod common;

use common::{build_site, read, visible_text, workspace, write};
use serde_json::json;

fn imported(root: &std::path::Path, description: Option<&str>) {
    write(
        root,
        "api/_collection.yaml",
        "name: Petstore\nsource:\n  format: openapi\n  location: petstore.json\n  sha256: '00'\n",
    );
    let description = description
        .map(|d| format!("description: {d}\n"))
        .unwrap_or_default();
    write(
        root,
        "api/list-users.rman.yaml",
        &format!(
            "version: 1\nkind: http\nname: listUsers\n{description}tags:\n  - openapi:operationId=listUsers\n  - openapi:path=/users\nmethod: GET\nurl: '{{{{base_url}}}}/users'\nquery:\n  - name: limit\n    value: '10'\n"
        ),
    );
    write(
        root,
        "api/create-user.rman.yaml",
        "version: 1\nkind: http\nname: createUser\ntags:\n  - openapi:path=/users\nmethod: POST\nurl: '{{base_url}}/users'\n",
    );
    let document = json!({
        "openapi": "3.1.0",
        "info": {"title": "Petstore", "version": "1"},
        "paths": {"/users": {
            "get": {
                "operationId": "listUsers",
                "summary": "All users",
                "description": "List users",
                "deprecated": true,
                "parameters": [{"name": "limit", "in": "query", "description": "Page size"}],
                "responses": {"200": {"description": "ok", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Users"}}}}}
            },
            "post": {
                "description": "Create one user",
                "requestBody": {"content": {"application/json": {"schema": {"type": "object", "required": ["name"]}}}},
                "responses": {"201": {"description": "created"}}
            }
        }},
        "components": {"schemas": {"Users": {"type": "array", "items": {"type": "string"}}}}
    });
    write(
        root,
        ".rustman/openapi/api.json",
        &serde_json::to_string_pretty(&document).unwrap(),
    );
}

#[test]
fn deprecated_operations_show_a_badge_on_the_page_and_in_navigation() {
    let ws = workspace("");
    imported(ws.path(), None);
    build_site(ws.path());
    let site = ws.path().join("site");
    let page = read(&site, "endpoints/api-list-users.html");
    assert!(
        page.contains("<h1>listUsers <span class=\"badge\">deprecated</span></h1>"),
        "{page}"
    );
    let index = read(&site, "index.html");
    let nav_entry = index.split("endpoints/api-list-users.html").nth(1).unwrap();
    assert!(nav_entry[..200].contains("deprecated"), "{nav_entry}");
    let create = read(&site, "endpoints/api-create-user.html");
    assert!(create.contains("<h1>createUser</h1>"), "{create}");
}

#[test]
fn the_operation_adds_summary_parameters_and_schemas() {
    let ws = workspace("");
    imported(ws.path(), None);
    build_site(ws.path());
    let site = ws.path().join("site");
    let text = visible_text(&read(&site, "endpoints/api-list-users.html"));
    assert!(text.contains("All users"), "{text}");
    assert!(text.contains("List users"), "{text}");
    assert!(
        text.contains("Name Value Description limit 10 Page size"),
        "{text}"
    );
    assert!(
        text.contains("Schemas Response 200 { &quot;type&quot;: &quot;array&quot;"),
        "{text}"
    );
    let create = visible_text(&read(&site, "endpoints/api-create-user.html"));
    assert!(
        create.contains("Create one user"),
        "matched by METHOD /path: {create}"
    );
    assert!(create.contains("Request body {"), "{create}");
}

#[test]
fn the_request_file_description_wins() {
    let ws = workspace("");
    imported(ws.path(), Some("Lists active users only"));
    build_site(ws.path());
    let text = visible_text(&read(
        &ws.path().join("site"),
        "endpoints/api-list-users.html",
    ));
    assert!(text.contains("Lists active users only"), "{text}");
    assert!(!text.contains("List users"), "{text}");
}

#[test]
fn without_a_snapshot_there_is_no_enrichment() {
    let ws = workspace("");
    imported(ws.path(), None);
    std::fs::remove_file(ws.path().join(".rustman/openapi/api.json")).unwrap();
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/api-list-users.html");
    assert!(
        !page.contains("deprecated") && !page.contains("All users"),
        "{page}"
    );
}
