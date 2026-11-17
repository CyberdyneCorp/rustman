use std::borrow::Cow;
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::model::{VarValue, Variables};
use crate::secret::Secret;
use crate::vars::{Layer, Lookup, VarSource, VariablesSource};

fn assertions(yaml: &str) -> Vec<Assertion> {
    crate::format::from_yaml(Path::new("a.yaml"), yaml).unwrap()
}

fn response(status: u16, body: &str) -> ResponseView<'static> {
    ResponseView {
        status,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Request-Id".into(), "7".into()),
            ("Vary".into(), "Accept".into()),
            ("vary".into(), "Origin".into()),
        ],
        body: Cow::Owned(body.as_bytes().to_vec()),
        binary: false,
        ttfb: Duration::from_millis(120),
        total: Duration::from_millis(350),
    }
}

fn captured(pairs: &[(&str, &str)]) -> VariablesSource<'static> {
    let vars = pairs
        .iter()
        .map(|(n, v)| ((*n).to_owned(), VarValue::String((*v).to_owned())))
        .collect();
    VariablesSource::owned(Layer::Capture, Variables::new(vars))
}

fn run_with(
    resolver: &Resolver<'_>,
    yaml: &str,
    response: &ResponseView<'_>,
) -> Vec<AssertionResult> {
    let redactor = Redactor::new();
    let ctx = Context {
        resolver,
        request: None,
        redactor: &redactor,
    };
    evaluate(&assertions(yaml), response, &ctx)
}

fn run(yaml: &str, response: &ResponseView<'_>) -> Vec<AssertionResult> {
    run_with(&Resolver::new(), yaml, response)
}

/// The one result of a single assertion, as shown to users.
fn shown(yaml: &str, response: &ResponseView<'_>) -> String {
    let results = run(yaml, response);
    assert_eq!(results.len(), 1);
    results[0].to_string()
}

#[test]
fn every_assertion_is_evaluated_after_a_failure() {
    let results = run(
        "- status: 200\n- header: {name: x-request-id, exists: true}\n- body: {contains: nope}\n",
        &response(404, "{}"),
    );
    let outcomes: Vec<Outcome> = results.iter().map(|r| r.outcome).collect();
    assert_eq!(outcomes, [Outcome::Fail, Outcome::Pass, Outcome::Fail]);
}

#[test]
fn status_forms_and_messages() {
    let r = response(201, "");
    assert_eq!(shown("- status: \"2xx\"\n", &r), "pass: status 2xx");
    assert_eq!(
        shown("- status: [200, 201]\n", &r),
        "pass: status [200, 201]"
    );
    assert_eq!(
        shown("- status: 200\n", &response(404, "")),
        "status: expected 200, got 404"
    );
    assert_eq!(
        shown("- status: [200, 204]\n", &r),
        "status: expected one of [200, 204], got 201"
    );
    assert_eq!(
        shown("- status: 4xx\n", &r),
        "status: expected 4xx, got 201"
    );
    let fail = &run("- status: 200\n", &response(404, ""))[0];
    assert_eq!(
        (fail.expected.as_deref(), fail.actual.as_deref()),
        (Some("200"), Some("404"))
    );
}

#[test]
fn header_assertions() {
    let r = response(200, "");
    assert_eq!(
        shown("- header: {name: x-request-id, exists: true}\n", &r),
        "pass: header x-request-id exists: true"
    );
    assert_eq!(
        shown("- header: {name: x-trace, exists: true}\n", &r),
        "header x-trace: expected to exist, got nothing"
    );
    assert_eq!(
        shown("- header: {name: x-request-id, exists: false}\n", &r),
        "header x-request-id: expected not to exist, got \"7\""
    );
    // A repeated header passes when any value satisfies it.
    assert!(run("- header: {name: VARY, equals: Origin}\n", &r)[0].passed());
    assert!(run("- header: {name: content-type, contains: json}\n", &r)[0].passed());
    assert!(
        run(
            "- header: {name: content-type, matches: '^application/'}\n",
            &r
        )[0]
        .passed()
    );
    assert_eq!(
        shown("- header: {name: vary, equals: Cookie}\n", &r),
        "header vary: expected \"Cookie\", got \"Accept\", \"Origin\""
    );
    assert_eq!(
        shown("- header: {name: vary, matches: '('}\n", &r),
        "error: header vary: invalid regular expression /(/: regex parse error:\n    (\n    ^\nerror: unclosed group"
    );
}

#[test]
fn sensitive_header_values_are_not_shown() {
    let mut r = response(200, "");
    r.headers.push(("Set-Cookie".into(), "sid=abc".into()));
    assert_eq!(
        shown("- header: {name: set-cookie, equals: x}\n", &r),
        "header set-cookie: expected \"x\", got \"«redacted:Set-Cookie»\""
    );
}

#[test]
fn body_assertions() {
    let r = response(200, "created order-123456");
    assert!(run("- body: {matches: \"order-[0-9]{6}\"}\n", &r)[0].passed());
    assert!(run("- body: {contains: order}\n", &r)[0].passed());
    assert!(run("- body: {equals: created order-123456}\n", &r)[0].passed());
    assert_eq!(
        shown("- body: {not_contains: order}\n", &r),
        "body: expected not to contain \"order\", got \"created order-123456\""
    );
    assert_eq!(
        shown("- body: {equals: x}\n", &r),
        "body: expected \"x\", got \"created order-123456\""
    );
    let mut binary = response(200, "");
    binary.binary = true;
    assert_eq!(
        shown("- body: {contains: x}\n", &binary),
        "error: body is binary"
    );
}

#[test]
fn long_bodies_are_cut_in_messages() {
    let r = response(200, &"a".repeat(1000));
    let result = &run("- body: {contains: b}\n", &r)[0];
    assert!(result.actual.as_ref().unwrap().len() < 220, "{result:?}");
}

#[test]
fn latency_assertions() {
    let r = response(200, "");
    assert_eq!(
        shown("- latency: {total_lt: 200}\n", &r),
        "latency total: expected < 200 ms, got 350 ms"
    );
    assert_eq!(
        shown("- latency: {ttfb_lt: 100}\n", &r),
        "latency ttfb: expected < 100 ms, got 120 ms"
    );
    assert_eq!(
        shown("- latency: {total_lt: 1000, ttfb_lt: 500}\n", &r),
        "pass: latency total < 1000 ms, ttfb < 500 ms"
    );
}

#[test]
fn json_equality_and_ops() {
    let r = response(
        200,
        r#"{"user":{"id":42,"name":"Ana","email":"ana@example.com"},"items":[1,2],"price":9.5,"tags":["a","b"],"none":null}"#,
    );
    let pass = [
        "- json: {path: \"$.user.id\", equals: 42}",
        "- json: {path: \"$.user.id\", equals: 42.0}",
        "- json: {path: $.user, equals: {id: 42, name: Ana, email: ana@example.com}}",
        "- json: {path: $.user.name, not_equals: Bob}",
        "- json: {path: $.user.email, contains: '@example'}",
        "- json: {path: $.tags, contains: b}",
        "- json: {path: $.user, contains: email}",
        "- json: {path: $.user.email, matches: '^[a-z]+@'}",
        "- json: {path: $.user.id, matches: '^4'}",
        "- json: {path: $.price, gt: 9}",
        "- json: {path: $.price, gte: 9.5}",
        "- json: {path: $.price, lt: 10}",
        "- json: {path: $.price, lte: 9.5}",
        "- json: {path: $.items, length: 2}",
        "- json: {path: $.user.name, length: 3}",
        "- json: {path: $.user.id, type: integer}",
        "- json: {path: $.price, type: number}",
        "- json: {path: $.none, type: 'null'}",
        "- json: {path: $.tags, type: array}",
        "- json: {path: $.none, exists: true}",
        "- json: {path: $.nope, exists: false}",
    ];
    for yaml in pass {
        let result = &run(&format!("{yaml}\n"), &r)[0];
        assert!(result.passed(), "{yaml}: {result}");
    }
    assert_eq!(
        shown("- json: {path: $.items, length: 3}\n", &r),
        "json $.items: expected length 3, got 2"
    );
    assert_eq!(
        shown("- json: {path: $.user.id, equals: 41}\n", &r),
        "json $.user.id: expected 41, got 42"
    );
    assert_eq!(
        shown("- json: {path: $.price, gt: 10}\n", &r),
        "json $.price: expected > 10, got 9.5"
    );
    assert_eq!(
        shown("- json: {path: $.user.name, type: number}\n", &r),
        "json $.user.name: expected type number, got string"
    );
    assert_eq!(
        shown("- json: {path: $.user.id, length: 1}\n", &r),
        "json $.user.id: expected length 1, got 42 (a number)"
    );
    assert_eq!(
        shown("- json: {path: $.user.id, type: text}\n", &r),
        "error: json $.user.id: unknown type 'text' (expected one of: string, number, integer, boolean, array, object, null)"
    );
}

#[test]
fn a_missing_path_fails_rather_than_errors() {
    let r = response(200, r#"{"a":1}"#);
    let result = &run("- json: {path: \"$.nope\", exists: true}\n", &r)[0];
    assert_eq!(result.outcome, Outcome::Fail);
    assert_eq!(
        result.message,
        "json $.nope: expected to exist, got nothing"
    );
    let result = &run("- json: {path: $.nope, equals: 1}\n", &r)[0];
    assert_eq!(result.outcome, Outcome::Fail);
    assert_eq!(result.message, "json $.nope: expected 1, got nothing");
}

#[test]
fn several_nodes_compare_as_a_list() {
    let r = response(200, r#"{"items":[{"id":1},{"id":2},{"id":3}]}"#);
    assert!(run("- json: {path: \"$.items[*].id\", equals: [1, 2, 3]}\n", &r)[0].passed());
    assert!(run("- json: {path: \"$.items[*].id\", length: 3}\n", &r)[0].passed());
    assert!(run("- json: {path: \"$.items[*].id\", contains: 2}\n", &r)[0].passed());
    assert!(run("- json: {path: \"$..id\", not_equals: [1, 2]}\n", &r)[0].passed());
    assert_eq!(
        shown(
            "- json: {path: \"$.items[?@.id > 1].id\", equals: [3]}\n",
            &r
        ),
        "json $.items[?@.id > 1].id: expected [3], got [2,3]"
    );
}

#[test]
fn json_errors() {
    assert_eq!(
        shown("- json: {path: $.a, equals: 1}\n", &response(200, "<html>")),
        "error: response body is not JSON"
    );
    let result = &run("- json: {path: \"$[\", equals: 1}\n", &response(200, "{}"))[0];
    assert_eq!(result.outcome, Outcome::Error);
    assert!(
        result.message.starts_with("json $[: invalid JSONPath"),
        "{result}"
    );
    assert_eq!(
        shown("- json: {path: $.a, gt: x}\n", &response(200, r#"{"a":1}"#)),
        "error: json $.a: 'gt' needs a number, got \"x\""
    );
}

#[test]
fn inline_schema_violations() {
    let schema = "- schema:\n    inline:\n      type: object\n      required: [email]\n      properties:\n        email: {type: string}\n";
    let ok = response(200, r#"{"email":"a@b.c"}"#);
    assert!(run(schema, &ok)[0].passed());
    let bad = response(200, r#"{"email":5}"#);
    let result = &run(schema, &bad)[0];
    assert_eq!(result.outcome, Outcome::Fail);
    assert!(
        result
            .message
            .contains(r#"/email: 5 is not of type "string""#),
        "{result}"
    );
    assert_eq!(
        result.message,
        r#"schema: expected a valid document, got 1 violation: /email: 5 is not of type "string""#
    );
    let missing = &run(schema, &response(200, "{}"))[0];
    assert_eq!(
        missing.actual.as_deref(),
        Some(r#"1 violation: /: "email" is a required property"#)
    );
}

#[test]
fn schema_drafts_and_violation_cap() {
    let draft7 = r#"- schema:
    inline:
      $schema: http://json-schema.org/draft-07/schema#
      type: array
      items: {type: string}
"#;
    let body = format!(
        "[{}]",
        (1..=12)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let result = &run(draft7, &response(200, &body))[0];
    let actual = result.actual.as_deref().unwrap();
    assert!(
        actual.starts_with("12 violations: /0: 1 is not of type \"string\";"),
        "{actual}"
    );
    assert_eq!(actual.matches(" is not of type").count(), MAX_VIOLATIONS);
    assert!(actual.ends_with("; and 2 more"), "{actual}");
    // In draft-07, `items` with an array is tuple validation; 2020-12 rejects it.
    let tuple = "- schema:\n    inline:\n      $schema: http://json-schema.org/draft-07/schema#\n      items: [{type: string}]\n";
    assert!(run(tuple, &response(200, r#"["a", 1]"#))[0].passed());
}

#[test]
fn schema_errors() {
    let r = response(200, "{}");
    assert_eq!(
        shown("- schema: {file: schemas/user.json}\n", &r),
        "error: schema: a schema file needs a workspace"
    );
    let result = &run("- schema: {inline: {type: 5}}\n", &r)[0];
    assert_eq!(result.outcome, Outcome::Error);
    assert!(
        result.message.starts_with("schema: invalid schema"),
        "{result}"
    );
    assert_eq!(
        shown(
            "- schema: {inline: {type: object}}\n",
            &response(200, "nope")
        ),
        "error: response body is not JSON"
    );
}

#[test]
fn expected_values_are_interpolated() {
    let resolver = Resolver::new().with(captured(&[
        ("user_id", "42"),
        ("name", "Ana"),
        ("min", "40"),
    ]));
    let r = response(200, r#"{"id":42,"name":"Ana","ok":true}"#);
    let yaml = r#"- json: {path: "$.id", equals: "{{user_id}}"}
- json: {path: $.name, equals: "{{name}}"}
- json: {path: $.id, gt: "{{min}}"}
- header: {name: x-request-id, equals: "{{user_id}}"}
- body: {contains: "{{name}}"}
- json: {path: $.ok, equals: "true"}
"#;
    let results = run_with(&resolver, yaml, &r);
    let outcomes: Vec<Outcome> = results.iter().map(|r| r.outcome).collect();
    // A literal "true" is a string, unlike the boolean.
    assert_eq!(
        outcomes,
        [
            Outcome::Pass,
            Outcome::Pass,
            Outcome::Pass,
            Outcome::Fail,
            Outcome::Pass,
            Outcome::Fail
        ]
    );
    let missing = run_with(
        &resolver,
        "- json: {path: $.id, equals: \"{{nope}} {{other}}\"}\n",
        &r,
    );
    assert_eq!(
        missing[0].to_string(),
        "error: json $.id: unresolved variable(s): nope, other"
    );
    let escaped = run_with(
        &resolver,
        "- body: {contains: \"\\\\{{name}}\"}\n",
        &response(200, "{{name}}"),
    );
    assert!(escaped[0].passed(), "{}", escaped[0]);
}

/// A secret value used in an expected value never appears in a result.
#[test]
fn secrets_in_expected_values_are_redacted() {
    struct Secrets;
    impl VarSource for Secrets {
        fn layer(&self) -> Layer {
            Layer::Environment
        }
        fn lookup(&self, name: &str) -> Option<Lookup> {
            (name == "token").then(|| Lookup::Secret(Secret::new("token", "canary-s3cr3t".into())))
        }
    }
    let resolver = Resolver::new().with(Secrets);
    let r = response(200, r#"{"token":"canary-s3cr3t-old"}"#);
    let results = run_with(
        &resolver,
        "- json: {path: $.token, equals: \"{{token}}\"}\n- body: {equals: \"{{token}}\"}\n",
        &r,
    );
    for result in &results {
        let all = format!("{result:?} {result}");
        assert!(!all.contains("canary-s3cr3t"), "{all}");
        assert!(all.contains("«redacted:token»"), "{all}");
    }
}

#[test]
fn needs_body_only_for_body_reading_assertions() {
    assert!(!needs_body(&assertions(
        "- status: 200\n- latency: {total_lt: 5}\n"
    )));
    assert!(needs_body(&assertions(
        "- status: 200\n- json: {path: $.a, exists: true}\n"
    )));
    assert!(needs_body(&assertions("- body: {contains: a}\n")));
}

#[test]
fn results_serialize_for_reports() {
    let results = run("- status: 200\n", &response(404, ""));
    assert_eq!(
        serde_json::to_value(&results[0]).unwrap(),
        serde_json::json!({
            "kind": "status", "outcome": "fail", "label": "status 200",
            "expected": "200", "actual": "404",
            "message": "status: expected 200, got 404"
        })
    );
}

#[test]
fn no_assertions_means_no_failures() {
    assert!(run("[]\n", &response(500, "")).is_empty());
}
