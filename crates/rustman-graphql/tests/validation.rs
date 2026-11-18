//! Pre-send validation, operation selection, variables and deprecations
//! (add-graphql-client tasks 3.1 to 3.4).

use rustman_core::model::{GraphqlMethod, GraphqlTransport};
use rustman_graphql::{
    Diagnostic, GraphqlSchema, Position, Severity, check_http_method, select_operation, validate,
};
use serde_json::{Map, Value, json};

const LIBRARY: &str = include_str!("fixtures/library.graphql");

fn schema() -> GraphqlSchema {
    GraphqlSchema::from_sdl(LIBRARY, "library.graphql").unwrap()
}

fn vars(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("an object")
    };
    map
}

fn shown(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(ToString::to_string).collect()
}

#[test]
fn an_unknown_field_blocks_the_send_with_its_position() {
    let schema = schema();
    let result = validate(
        "{ user(id: 1) { emial } }",
        Some(&schema),
        None,
        &Map::new(),
    );
    assert!(result.has_errors());
    assert!(!result.syntax_only);
    let errors: Vec<_> = result.errors().collect();
    assert_eq!(errors.len(), 1, "only the unknown field: {errors:?}");
    let unknown = errors[0];
    assert_eq!(
        unknown.message,
        r#"Cannot query field "emial" on type "User"."#
    );
    assert_eq!(
        unknown.position,
        Some(Position {
            line: 1,
            column: 17
        })
    );
    assert_eq!(
        unknown.to_string(),
        r#"1:17: Cannot query field "emial" on type "User"."#
    );
}

#[test]
fn a_composite_field_without_a_selection_set_is_still_an_error() {
    let schema = schema();
    for document in ["{ user(id: 1) }", "{ user(id: 1) @include(if: true) }"] {
        let result = validate(document, Some(&schema), None, &Map::new());
        let messages: Vec<String> = result.errors().map(|d| d.message.clone()).collect();
        assert_eq!(messages.len(), 1, "{document}: {messages:?}");
        assert!(
            messages[0].contains("must have a selection of subfields"),
            "{messages:?}"
        );
    }
}

#[test]
fn every_error_is_reported_with_line_and_column() {
    let schema = schema();
    let document = "query {\n  user(id: 1) {\n    nope\n  }\n  missing\n}";
    let result = validate(document, Some(&schema), None, &Map::new());
    let errors: Vec<&Diagnostic> = result.errors().collect();
    let positions: Vec<Option<Position>> = errors.iter().map(|d| d.position).collect();
    assert!(
        positions.contains(&Some(Position { line: 3, column: 5 })),
        "{errors:?}"
    );
    assert!(
        positions.contains(&Some(Position { line: 5, column: 3 })),
        "{errors:?}"
    );
}

#[test]
fn without_a_schema_only_syntax_is_checked() {
    let unknown_field = validate("{ user(id: 1) { emial } }", None, None, &Map::new());
    assert!(unknown_field.syntax_only);
    assert!(unknown_field.diagnostics.is_empty());

    let missing_brace = validate("{ user(id: 1) { name }", None, None, &Map::new());
    assert!(missing_brace.has_errors());
    assert_eq!(
        shown(&missing_brace.diagnostics),
        ["1:23: syntax error: expected R_CURLY, got EOF"]
    );
}

#[test]
fn a_valid_document_has_no_diagnostics() {
    let schema = schema();
    let document = "query U($id: ID!) { user(id: $id) { name handle books { title } } }";
    let result = validate(document, Some(&schema), None, &vars(json!({"id": "1"})));
    assert_eq!(result.diagnostics, []);
}

#[test]
fn an_ambiguous_document_names_its_operation_count() {
    let document = "query A { users { id } }\nquery B { users { name } }";
    let err = select_operation(document, None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "graphql: document has 2 operations; set operation_name"
    );
    assert_eq!(
        select_operation(document, Some("B"))
            .unwrap()
            .name
            .as_deref(),
        Some("B")
    );
}

#[test]
fn a_mutation_over_get_is_rejected() {
    let op = select_operation(
        "mutation { login(email: \"a\", password: \"b\") { token } }",
        None,
    )
    .unwrap();
    let err = check_http_method(&op, GraphqlTransport::Http, GraphqlMethod::Get).unwrap_err();
    assert_eq!(err.to_string(), "graphql: mutations cannot use GET");
}

#[test]
fn a_missing_required_variable_blocks_the_send() {
    let schema = schema();
    let result = validate(
        "query($id: ID!) { user(id: $id) { name } }",
        Some(&schema),
        None,
        &Map::new(),
    );
    assert!(result.has_errors());
    assert_eq!(
        shown(&result.diagnostics),
        ["variable $id of type ID! is required"]
    );
}

#[test]
fn variables_of_the_selected_operation_are_checked() {
    let schema = schema();
    let document = "query A($id: ID!) { user(id: $id) { name } }\n\
                    mutation B($input: CreateUserInput!) { createUser(input: $input) { id } }";
    let input = vars(json!({"input": {"name": 1, "role": "OWNER"}}));
    let result = validate(document, Some(&schema), Some("B"), &input);
    assert_eq!(
        shown(&result.diagnostics),
        [
            "variable $input.name: expected String, got number",
            "variable $input.role: expected Role, got string",
        ]
    );
}

#[test]
fn deprecated_fields_and_enum_values_warn_without_blocking() {
    let schema = schema();
    let document = "{ user(id: 1) { username } users(role: GUEST) { id } }";
    let result = validate(document, Some(&schema), None, &Map::new());
    assert!(!result.has_errors());
    let warnings: Vec<&Diagnostic> = result.warnings().collect();
    assert_eq!(
        warnings
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>(),
        [
            "User.username is deprecated: Use handle",
            "Role.GUEST is deprecated: Guests were removed",
        ]
    );
    assert!(warnings.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(
        warnings[0].to_string(),
        "1:17: User.username is deprecated: Use handle"
    );
}

#[test]
fn deprecations_inside_fragments_lists_and_inputs_are_found() {
    let sdl = "type Query { a(roles: [R!], input: In): T }\n\
               type T { old: Int @deprecated }\n\
               enum R { X Y @deprecated(reason: \"no Y\") }\n\
               input In { r: R }";
    let schema = GraphqlSchema::from_sdl(sdl, "s.graphql").unwrap();
    let document =
        "{ ...F } fragment F on Query { a(roles: [X, Y], input: {r: Y}) { ... on T { old } } }";
    let result = validate(document, Some(&schema), None, &Map::new());
    let messages: Vec<String> = result.warnings().map(|d| d.message.clone()).collect();
    assert_eq!(
        messages,
        [
            "R.Y is deprecated: no Y",
            "R.Y is deprecated: no Y",
            "T.old is deprecated: No longer supported",
        ]
    );
}

#[test]
fn deprecated_enum_values_in_variable_defaults_warn() {
    let sdl = "type Query { a(r: Role, input: In): Int }\n\
               enum Role { A B @deprecated(reason: \"gone\") }\n\
               input In { roles: [Role!] }";
    let schema = GraphqlSchema::from_sdl(sdl, "s.graphql").unwrap();
    let document = "query($r: Role = B, $i: In = {roles: [A, B]}) { a(r: $r, input: $i) }";
    let result = validate(document, Some(&schema), None, &Map::new());
    assert!(!result.has_errors(), "{:?}", shown(&result.diagnostics));
    let warnings: Vec<String> = result.warnings().map(ToString::to_string).collect();
    assert_eq!(
        warnings,
        [
            "1:18: Role.B is deprecated: gone",
            "1:42: Role.B is deprecated: gone"
        ]
    );
}
