//! Request-file schema (task 2.3): fixture loading and exact error positions.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::LoadError;
use rustman_core::model::{Body, JsonContent, KindRegistry, Request};

fn fixture(dir: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir)
        .join(name)
}

fn load(dir: &str, name: &str) -> Result<Request, LoadError> {
    let path = fixture(dir, name);
    let text = fs::read_to_string(&path).unwrap();
    KindRegistry::default().load(Path::new(name), &text)
}

fn http(name: &str) -> Box<rustman_core::model::HttpRequest> {
    match load("requests", name).unwrap() {
        Request::Http(http) => http,
        other => panic!("expected http, got {other:?}"),
    }
}

fn error(name: &str) -> LoadError {
    load("invalid", name).unwrap_err()
}

#[test]
fn every_valid_fixture_loads() {
    for entry in fs::read_dir(fixture("requests", "")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        load("requests", &name).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn minimal_request_is_available_by_name() {
    let request = http("minimal.rman.yaml");
    assert_eq!(request.name, "Get user");
    assert_eq!(request.effective_method(), "GET");
    assert_eq!(request.url, "{{base_url}}/users/1");
}

#[test]
fn method_defaults_to_get() {
    let text = "version: 1\nkind: http\nname: x\nurl: http://x\n";
    let Request::Http(request) = KindRegistry::default().load(Path::new("x"), text).unwrap() else {
        panic!("expected http");
    };
    assert!(request.method.is_none());
    assert_eq!(request.effective_method(), "GET");
}

#[test]
fn custom_method_and_repeated_query_keep_order() {
    let request = http("custom-method.rman.yaml");
    assert_eq!(request.effective_method(), "PURGE");
    let query: Vec<_> = request
        .query
        .iter()
        .flatten()
        .map(|q| (q.name.as_str(), q.value.as_str()))
        .collect();
    assert_eq!(query, [("tag", "a"), ("tag", "b")]);
}

#[test]
fn yaml_1_1_scalars_in_a_json_body_stay_strings() {
    let request = http("json-body.rman.yaml");
    let Some(Body::Json(JsonContent::Value(content))) = &request.body else {
        panic!("expected a json body");
    };
    assert_eq!(content["answers"], serde_json::json!(["no", "on", "0755"]));
    assert_eq!(content["age"], serde_json::json!(3));
    assert_eq!(content["admin"], serde_json::json!(true));
    let keys: Vec<_> = content.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "name", "age", "ratio", "admin", "nickname", "answers", "nested"
        ]
    );
}

#[test]
fn unquoted_yaml_1_1_scalars_also_stay_strings() {
    let text = "version: 1\nkind: http\nname: x\nurl: http://x\nbody:\n  type: json\n  content: {a: no, b: on, c: 0755, d: 1_000}\n";
    let Request::Http(request) = KindRegistry::default().load(Path::new("x"), text).unwrap() else {
        panic!("expected http");
    };
    let Some(Body::Json(JsonContent::Value(content))) = request.body else {
        panic!("expected a json body");
    };
    assert_eq!(
        content,
        serde_json::json!({"a": "no", "b": "on", "c": "0755", "d": "1_000"})
    );
}

#[test]
fn a_quoted_version_is_rejected() {
    let text = "version: '1'\nkind: http\nname: x\nurl: http://x\n";
    let err = KindRegistry::default()
        .load(Path::new("x"), text)
        .unwrap_err();
    assert_eq!(err.line(), Some(1), "{err}");
    assert!(
        err.to_string().contains("unquoted integer version"),
        "{err}"
    );
}

#[test]
fn reserved_kinds_load_and_report_unsupported() {
    let request = load("requests", "graphql.rman.yaml").unwrap();
    assert_eq!(request.kind(), "graphql");
    assert_eq!(request.name(), "List users");
    assert_eq!(
        request.unsupported_reason().as_deref(),
        Some("request kind 'graphql' is not supported by this build")
    );
    let mcp = "version: 1\nkind: mcp\nname: Call tool\ntool: echo\n";
    let request = KindRegistry::default().load(Path::new("m"), mcp).unwrap();
    assert_eq!(request.kind(), "mcp");
}

#[test]
fn registry_accepts_new_kinds() {
    let mut registry = KindRegistry::default();
    registry.register("jsonrpc", rustman_core::model::kind::load_opaque);
    assert_eq!(registry.names(), ["http", "graphql", "mcp", "jsonrpc"]);
    let text = "version: 1\nkind: jsonrpc\nname: Block number\n";
    assert_eq!(
        registry.load(Path::new("j"), text).unwrap().kind(),
        "jsonrpc"
    );
}

fn assert_error(err: &LoadError, line: usize, column: usize, text: &str) {
    assert_eq!(
        (err.line(), err.column()),
        (Some(line), Some(column)),
        "{err}"
    );
    assert_eq!(err.to_string(), text);
}

#[test]
fn unknown_kind_names_file_line_and_registered_kinds() {
    let err = error("unknown-kind.rman.yaml");
    assert_error(
        &err,
        2,
        7,
        "unknown-kind.rman.yaml:2: unknown request kind 'soap' (expected one of: http, graphql, mcp)",
    );
}

#[test]
fn invalid_method_token_is_rejected() {
    let err = error("invalid-method.rman.yaml");
    assert_error(
        &err,
        5,
        9,
        "invalid-method.rman.yaml:5: invalid HTTP method 'GET USERS'",
    );
}

#[test]
fn missing_kind_is_rejected() {
    let err = error("missing-kind.rman.yaml");
    assert_error(
        &err,
        1,
        1,
        "missing-kind.rman.yaml:1: missing request kind (expected one of: http, graphql, mcp)",
    );
}

#[test]
fn future_version_is_rejected() {
    let err = error("future-version.rman.yaml");
    assert_error(
        &err,
        1,
        10,
        "future-version.rman.yaml:1: unsupported request file version 2 (expected 1)",
    );
}

#[test]
fn unknown_field_has_exact_position() {
    let err = error("unknown-field.rman.yaml");
    assert_eq!((err.line(), err.column()), (Some(5), Some(1)), "{err}");
    assert_eq!(
        err.message,
        "unknown field `header`, expected one of version, kind, name, description, tags, method, url, query, headers, body, auth, settings, variables, assertions, captures"
    );
}

#[test]
fn duplicate_key_has_exact_position() {
    let err = error("duplicate-key.rman.yaml");
    assert_eq!((err.line(), err.column()), (Some(5), Some(1)), "{err}");
    assert!(
        err.message.starts_with("duplicate mapping key: name"),
        "{err}"
    );
}

#[test]
fn bad_indentation_has_exact_position() {
    let err = error("bad-indent.rman.yaml");
    assert_error(
        &err,
        5,
        9,
        "bad-indent.rman.yaml:5: mapping values are not allowed in this context",
    );
}

#[test]
fn wrong_type_has_exact_position() {
    let err = error("wrong-type.rman.yaml");
    assert_eq!((err.line(), err.column()), (Some(6), Some(15)), "{err}");
}

#[test]
fn unclosed_quote_has_exact_position() {
    let err = error("unclosed-quote.rman.yaml");
    assert_error(&err, 4, 6, "unclosed-quote.rman.yaml:4: unclosed quote");
}
