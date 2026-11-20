//! The normalized model (task 1.2): operations matched by template whatever
//! their parameter names, merged path-item parameters, 3.0 `nullable`,
//! recursive schemas and `$ref` refactors; and the spec scenario "Two
//! versions compared".

mod common;

use rustman_contract::{Classification, RuleId};

use common::{pair, rule_ids};

fn expected(name: &str) -> common::Pair {
    let pair = pair(&format!("model/{name}.yaml"));
    assert_eq!(rule_ids(&pair.diff), pair.expect, "{:#?}", pair.diff);
    pair
}

#[test]
fn a_renamed_path_parameter_is_the_same_operation() {
    let pair = expected("renamed-path-parameter");
    let renamed = &pair.diff.differences[0];
    assert_eq!(renamed.operation, "GET /users/{userId}");
    assert_eq!(renamed.message, "path parameter renamed: {id} -> {userId}");
    assert_eq!(renamed.old_pointer, "/paths/~1users~1{id}");
    assert_eq!(renamed.new_pointer, "/paths/~1users~1{userId}");
}

#[test]
fn shared_parameters_are_merged_before_comparing() {
    expected("shared-parameters");
}

#[test]
fn nullable_is_the_same_as_a_null_type() {
    expected("nullable");
}

#[test]
fn a_recursive_schema_is_compared_once() {
    let pair = expected("recursive-schema");
    let removed = &pair.diff.differences[0];
    assert_eq!(
        removed.message,
        "response 200 (application/json) $.label: removed"
    );
    assert_eq!(
        removed.old_pointer,
        "/components/schemas/Node/properties/label"
    );
    assert_eq!(removed.new_pointer, "/components/schemas/Node/properties");
}

#[test]
fn moving_a_schema_into_components_is_not_a_change() {
    expected("ref-refactor");
}

#[test]
fn two_versions_compared() {
    let pair = expected("two-versions");
    let removed = &pair.diff.differences[0];
    assert_eq!(removed.rule, RuleId::OperationRemoved);
    assert_eq!(removed.classification, Classification::Breaking);
    assert_eq!(removed.operation, "DELETE /users/{id}");
    assert_eq!(removed.old_pointer, "/paths/~1users~1{id}/delete");
    assert_eq!(removed.new_pointer, "/paths/~1users~1{id}");
    let added = &pair.diff.differences[1];
    assert_eq!(added.rule, RuleId::OperationAdded);
    assert_eq!(added.classification, Classification::NonBreaking);
    assert_eq!(added.operation, "GET /health");
    assert_eq!(added.old_pointer, "/paths");
    assert_eq!(added.new_pointer, "/paths/~1health/get");
    assert_eq!(pair.diff.summary(), "1 breaking, 1 non-breaking, 0 unknown");
}

#[test]
fn the_diff_is_deterministic() {
    let first = pair("model/two-versions.yaml").diff;
    let second = pair("model/two-versions.yaml").diff;
    assert_eq!(first, second);
    assert_eq!(first.catalog_version, rustman_contract::CATALOG_VERSION);
}
