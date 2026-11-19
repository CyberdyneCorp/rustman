//! One fixture pair and one test per rule id (tasks 2.2 and 2.3): each pair
//! in `fixtures/contract/rules/<rule-id>.yaml` reports exactly its expected
//! rules, with the catalog classification.

mod common;

use rustman_contract::RuleId;

use common::{fixtures, pair, rule_ids};

fn check(id: &str) {
    let rule = RuleId::from_id(id).unwrap_or_else(|| panic!("no rule {id}"));
    let pair = pair(&format!("rules/{id}.yaml"));
    assert_eq!(rule_ids(&pair.diff), pair.expect, "{:#?}", pair.diff);
    let reported = pair
        .diff
        .differences
        .iter()
        .find(|d| d.rule == rule)
        .unwrap();
    assert_eq!(reported.classification, rule.classification());
    assert!(!reported.old_pointer.is_empty() && !reported.new_pointer.is_empty());
}

macro_rules! rule_tests {
    ($($name:ident => $id:literal,)*) => {
        $(
            #[test]
            fn $name() {
                check($id);
            }
        )*

        #[test]
        fn every_rule_has_a_test_and_a_fixture() {
            let tested = [$($id,)*];
            for rule in RuleId::ALL {
                assert!(tested.contains(&rule.id()), "no test for {}", rule.id());
            }
            let files: Vec<String> = std::fs::read_dir(fixtures().join("rules"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(files.len(), RuleId::ALL.len(), "{files:?}");
        }
    };
}

rule_tests! {
    constraint_changed => "constraint-changed",
    response_header_added => "response-header-added",
    response_header_became_required => "response-header-became-required",
    operation_removed => "operation-removed",
    response_status_removed => "response-status-removed",
    response_media_type_removed => "response-media-type-removed",
    response_field_removed => "response-field-removed",
    response_field_became_optional => "response-field-became-optional",
    response_field_type_changed => "response-field-type-changed",
    response_header_removed => "response-header-removed",
    response_header_became_optional => "response-header-became-optional",
    response_enum_narrowed => "response-enum-narrowed",
    request_required_field_added => "request-required-field-added",
    request_field_removed_from_closed_schema => "request-field-removed-from-closed-schema",
    request_required_parameter_added => "request-required-parameter-added",
    parameter_became_required => "parameter-became-required",
    request_body_became_required => "request-body-became-required",
    request_media_type_removed => "request-media-type-removed",
    request_type_changed => "request-type-changed",
    request_enum_narrowed => "request-enum-narrowed",
    request_constraint_tightened => "request-constraint-tightened",
    parameter_location_changed => "parameter-location-changed",
    security_requirement_removed => "security-requirement-removed",
    operation_added => "operation-added",
    operation_deprecated => "operation-deprecated",
    path_parameter_renamed => "path-parameter-renamed",
    response_status_added => "response-status-added",
    response_media_type_added => "response-media-type-added",
    response_field_added => "response-field-added",
    response_field_became_required => "response-field-became-required",
    response_type_narrowed => "response-type-narrowed",
    response_constraint_tightened => "response-constraint-tightened",
    request_optional_field_added => "request-optional-field-added",
    request_optional_parameter_added => "request-optional-parameter-added",
    request_field_became_optional => "request-field-became-optional",
    parameter_became_optional => "parameter-became-optional",
    request_body_added => "request-body-added",
    request_body_became_optional => "request-body-became-optional",
    request_media_type_added => "request-media-type-added",
    request_type_widened => "request-type-widened",
    request_enum_widened => "request-enum-widened",
    request_constraint_loosened => "request-constraint-loosened",
    security_alternative_added => "security-alternative-added",
    docs_changed => "docs-changed",
    response_enum_widened => "response-enum-widened",
    response_constraint_loosened => "response-constraint-loosened",
    response_field_added_to_closed_schema => "response-field-added-to-closed-schema",
    request_field_removed => "request-field-removed",
    parameter_removed => "parameter-removed",
    request_body_removed => "request-body-removed",
    security_alternative_removed => "security-alternative-removed",
    composition_changed => "composition-changed",
    vendor_extension_changed => "vendor-extension-changed",
}
