//! The direction-aware walker (task 2.1): the same schema change is
//! classified by where the schema is. A stricter request breaks clients, a
//! stricter response does not; a removed response field breaks clients, a
//! removed request field is `unknown`.

mod common;

use serde_json::{Value, json};

use common::{diff_values, rule_ids};

/// A document whose `POST /users` takes `request` and returns `response`.
fn doc(request: Value, response: Value) -> Value {
    json!({"paths": {"/users": {"post": {
        "requestBody": {"content": {"application/json": {"schema": request}}},
        "responses": {"200": {"description": "ok", "content": {"application/json": {"schema": response}}}}
    }}}})
}

fn obj(properties: Value) -> Value {
    json!({"type": "object", "properties": properties})
}

#[test]
fn a_stricter_request_breaks_and_a_stricter_response_does_not() {
    let loose = obj(json!({"name": {"type": "string", "maxLength": 100}}));
    let strict = obj(json!({"name": {"type": "string", "maxLength": 10}}));
    let request = diff_values(
        doc(loose.clone(), json!({})),
        doc(strict.clone(), json!({})),
    );
    assert_eq!(rule_ids(&request), ["request-constraint-tightened"]);
    assert_eq!(
        request.differences[0].message,
        "request body (application/json) $.name: maxLength 100 -> 10"
    );
    let response = diff_values(doc(json!({}), loose), doc(json!({}), strict));
    assert_eq!(rule_ids(&response), ["response-constraint-tightened"]);
}

#[test]
fn a_removed_response_field_breaks_and_a_removed_request_field_is_unknown() {
    let full = obj(json!({"id": {"type": "integer"}, "email": {"type": "string"}}));
    let less = obj(json!({"id": {"type": "integer"}}));
    let response = diff_values(doc(json!({}), full.clone()), doc(json!({}), less.clone()));
    assert_eq!(rule_ids(&response), ["response-field-removed"]);
    assert_eq!(
        response.differences[0].message,
        "response 200 (application/json) $.email: removed"
    );
    assert_eq!(
        response.differences[0].old_pointer,
        "/paths/~1users/post/responses/200/content/application~1json/schema/properties/email"
    );
    let request = diff_values(doc(full, json!({})), doc(less, json!({})));
    assert_eq!(rule_ids(&request), ["request-field-removed"]);
}

#[test]
fn types_are_compared_by_direction() {
    let int = obj(json!({"age": {"type": "integer"}}));
    let num = obj(json!({"age": {"type": "number"}}));
    let widened = diff_values(doc(int.clone(), int.clone()), doc(num.clone(), num.clone()));
    assert_eq!(
        rule_ids(&widened),
        ["request-type-widened", "response-field-type-changed"]
    );
    let narrowed = diff_values(doc(num.clone(), num), doc(int.clone(), int));
    assert_eq!(
        rule_ids(&narrowed),
        ["request-type-changed", "response-type-narrowed"]
    );
}

#[test]
fn nested_items_and_additional_properties_are_walked() {
    let old = obj(json!({
        "tags": {"type": "array", "items": {"type": "string", "enum": ["a", "b"]}},
        "meta": {"type": "object", "additionalProperties": {"type": "string"}}
    }));
    let new = obj(json!({
        "tags": {"type": "array", "items": {"type": "string", "enum": ["a"]}},
        "meta": {"type": "object", "additionalProperties": {"type": "integer"}}
    }));
    let diff = diff_values(doc(json!({}), old), doc(json!({}), new));
    let messages: Vec<&str> = diff
        .differences
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "response 200 (application/json) $.meta.*: type string -> integer",
            "response 200 (application/json) $.tags[]: enum values removed: [b]",
        ]
    );
}

#[test]
fn parameter_schemas_are_request_schemas() {
    let with = |schema: Value| {
        json!({"paths": {"/users": {"get": {
            "parameters": [{"name": "limit", "in": "query", "schema": schema}],
            "responses": {"200": {"description": "ok"}}
        }}}})
    };
    let diff = diff_values(
        with(json!({"type": "integer", "maximum": 100})),
        with(json!({"type": "integer", "maximum": 50})),
    );
    assert_eq!(rule_ids(&diff), ["request-constraint-tightened"]);
    assert_eq!(
        diff.differences[0].message,
        "query parameter 'limit': maximum 100 -> 50"
    );
}

#[test]
fn all_of_members_add_up_to_one_object() {
    let with = |base: Value| json!({"allOf": [base, {"type": "object", "properties": {"id": {"type": "integer"}}}]});
    let old = with(obj(
        json!({"name": {"type": "string"}, "tag": {"type": "string"}}),
    ));
    let new = with(obj(json!({"name": {"type": "string"}})));
    let diff = diff_values(doc(json!({}), old), doc(json!({}), new));
    assert_eq!(rule_ids(&diff), ["response-field-removed"]);
    assert_eq!(
        diff.differences[0].message,
        "response 200 (application/json) $.tag: removed"
    );
}

#[test]
fn other_composition_changes_are_unknown() {
    let a = json!({"type": "string"});
    let b = json!({"type": "integer"});
    let one_of = |list: Vec<Value>| json!({"oneOf": list});
    let edited = diff_values(
        doc(json!({}), one_of(vec![a.clone(), b])),
        doc(
            json!({}),
            one_of(vec![a.clone(), json!({"type": "boolean"})]),
        ),
    );
    assert_eq!(rule_ids(&edited), ["composition-changed"]);
    assert_eq!(
        edited.differences[0].message,
        "response 200 (application/json): oneOf changed (2 schemas)"
    );
    let all_of = diff_values(
        doc(json!({}), json!({"allOf": [a.clone()]})),
        doc(json!({}), json!({"allOf": [a, {"pattern": "^a"}]})),
    );
    assert_eq!(rule_ids(&all_of), ["composition-changed"]);
    assert_eq!(
        all_of.differences[0].message,
        "response 200 (application/json): allOf 1 schema -> 2 schemas"
    );
}

fn member(name: &str) -> Value {
    json!({"type": "object", "properties": {name: {"type": "string"}}, "required": [name]})
}

#[test]
fn a_field_removed_with_an_all_of_member_breaks() {
    let old = json!({"allOf": [member("id"), member("email")]});
    let new = json!({"allOf": [member("id")]});
    let diff = diff_values(doc(json!({}), old), doc(json!({}), new));
    assert_eq!(rule_ids(&diff), ["response-field-removed"]);
    assert_eq!(
        diff.differences[0].message,
        "response 200 (application/json) $.email: removed"
    );
    assert_eq!(
        diff.differences[0].old_pointer,
        "/paths/~1users/post/responses/200/content/application~1json/schema/allOf/1/properties/email"
    );
}

#[test]
fn wrapping_a_schema_in_an_equivalent_all_of_is_not_a_change() {
    let plain = member("id");
    let wrapped = json!({"allOf": [member("id")]});
    let diff = diff_values(
        doc(plain.clone(), plain.clone()),
        doc(wrapped.clone(), wrapped),
    );
    assert_eq!(rule_ids(&diff), Vec::<String>::new());
    let moved = json!({"allOf": [{"type": "object"}, member("id")]});
    let diff = diff_values(doc(json!({}), plain), doc(json!({}), moved));
    assert_eq!(rule_ids(&diff), Vec::<String>::new());
}

#[test]
fn fields_of_all_of_members_are_walked() {
    let old = json!({"allOf": [{"type": "object", "properties": {"n": {"type": "integer", "maximum": 10}}}]});
    let new = json!({"type": "object", "properties": {"n": {"type": "integer", "maximum": 5}}});
    let diff = diff_values(doc(old, json!({})), doc(new, json!({})));
    assert_eq!(rule_ids(&diff), ["request-constraint-tightened"]);
    assert_eq!(
        diff.differences[0].message,
        "request body (application/json) $.n: maximum 10 -> 5"
    );
}

#[test]
fn a_field_removed_from_a_closed_request_breaks() {
    let closed = |properties: Value| json!({"type": "object", "additionalProperties": false, "properties": properties});
    let old = closed(json!({"a": {"type": "string"}, "b": {"type": "string"}}));
    let new = closed(json!({"a": {"type": "string"}}));
    let diff = diff_values(doc(old, json!({})), doc(new, json!({})));
    assert_eq!(
        rule_ids(&diff),
        ["request-field-removed-from-closed-schema"]
    );
    assert_eq!(
        diff.differences[0].message,
        "request body (application/json) $.b: removed"
    );
}

#[test]
fn response_headers_are_compared_in_the_response_direction() {
    let with = |headers: Value| json!({"paths": {"/users": {"get": {"responses": {"200": {"description": "ok", "headers": headers}}}}}});
    let old = with(json!({"X-Rate": {"required": true, "schema": {"type": "integer"}}}));
    let new = with(json!({}));
    let diff = diff_values(old.clone(), new);
    assert_eq!(rule_ids(&diff), ["response-header-removed"]);
    assert_eq!(
        diff.differences[0].message,
        "response 200 header 'X-Rate': removed"
    );
    let retyped =
        with(json!({"x-rate": {"required": true, "schema": {"type": ["integer", "string"]}}}));
    let diff = diff_values(old, retyped);
    assert_eq!(rule_ids(&diff), ["response-field-type-changed"]);
    assert_eq!(
        diff.differences[0].message,
        "response 200 header 'X-Rate': type integer -> [integer, string]"
    );
}

/// The messages of a diff, in order.
fn messages(diff: &rustman_contract::Diff) -> Vec<&str> {
    diff.differences
        .iter()
        .map(|d| d.message.as_str())
        .collect()
}

fn named(extra: Value) -> Value {
    let mut schema = obj(json!({"name": {"type": "string"}}));
    schema
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    schema
}

/// Regression: `additionalProperties` becoming `false` or a schema in a
/// request body reported no difference (review pairs a1 -> b1, a1 -> b2).
#[test]
fn a_request_that_closes_or_restricts_additional_properties_breaks() {
    let open = named(json!({}));
    let closed = named(json!({"additionalProperties": false}));
    let integers = named(json!({"additionalProperties": {"type": "integer"}}));
    let id = obj(json!({"id": {"type": "integer"}}));
    let case = |old: &Value, new: &Value| {
        diff_values(doc(old.clone(), id.clone()), doc(new.clone(), id.clone()))
    };
    let diff = case(&open, &closed);
    assert_eq!(rule_ids(&diff), ["request-constraint-tightened"]);
    assert_eq!(
        messages(&diff),
        ["request body (application/json): additionalProperties none -> false"]
    );
    assert_eq!(
        diff.differences[0].new_pointer,
        "/paths/~1users/post/requestBody/content/application~1json/schema/additionalProperties"
    );
    let diff = case(&open, &integers);
    assert_eq!(rule_ids(&diff), ["request-constraint-tightened"]);
    assert_eq!(
        messages(&diff),
        [r#"request body (application/json): additionalProperties none -> {"type":"integer"}"#]
    );
    // Boolean <-> schema, both ways.
    assert_eq!(
        rule_ids(&case(&integers, &closed)),
        ["request-constraint-tightened"]
    );
    assert_eq!(
        rule_ids(&case(&closed, &integers)),
        ["request-constraint-loosened"]
    );
    assert_eq!(
        rule_ids(&case(&closed, &open)),
        ["request-constraint-loosened"]
    );
    assert_eq!(
        rule_ids(&case(&integers, &open)),
        ["request-constraint-loosened"]
    );
    let all_open = named(json!({"additionalProperties": true}));
    assert_eq!(
        rule_ids(&case(&all_open, &closed)),
        ["request-constraint-tightened"]
    );
    assert_eq!(rule_ids(&case(&all_open, &open)), Vec::<String>::new());
    let empty = named(json!({"additionalProperties": {"description": "anything"}}));
    assert_eq!(rule_ids(&case(&open, &empty)), Vec::<String>::new());
    // A schema on both sides is compared member by member.
    let strings = named(json!({"additionalProperties": {"type": "string"}}));
    assert_eq!(
        messages(&case(&strings, &integers)),
        ["request body (application/json) $.*: type string -> integer"]
    );
}

/// Regression: a response schema that stopped being closed (review pair
/// a3 -> b3) reported no difference.
#[test]
fn a_response_that_stops_being_closed_is_unknown() {
    let closed = obj(json!({"id": {"type": "integer"}}));
    let closed = {
        let mut c = closed;
        c["additionalProperties"] = json!(false);
        c
    };
    let open = obj(json!({"id": {"type": "integer"}}));
    let name = named(json!({}));
    let diff = diff_values(
        doc(name.clone(), closed.clone()),
        doc(name.clone(), open.clone()),
    );
    assert_eq!(rule_ids(&diff), ["response-constraint-loosened"]);
    assert_eq!(
        diff.differences[0].classification,
        rustman_contract::Classification::Unknown
    );
    assert_eq!(
        messages(&diff),
        ["response 200 (application/json): additionalProperties false -> none"]
    );
    let diff = diff_values(doc(name.clone(), open), doc(name, closed));
    assert_eq!(rule_ids(&diff), ["response-constraint-tightened"]);
}

#[test]
fn an_all_of_member_that_closes_the_object_counts() {
    let old = json!({"allOf": [named(json!({}))]});
    let new = json!({"allOf": [named(json!({"additionalProperties": false}))]});
    let diff = diff_values(doc(old, json!({})), doc(new, json!({})));
    assert_eq!(rule_ids(&diff), ["request-constraint-tightened"]);
    assert_eq!(
        diff.differences[0].new_pointer,
        "/paths/~1users/post/requestBody/content/application~1json/schema/allOf/0/additionalProperties"
    );
}

/// Regression: `items` appearing or disappearing (review pairs b5 -> c5
/// and a5 -> b5) reported no difference.
#[test]
fn items_added_or_removed_are_classified_by_direction() {
    let bare = json!({"type": "array"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let integers = json!({"type": "array", "items": {"type": "integer"}});
    let request = |old: &Value, new: &Value| {
        diff_values(doc(old.clone(), json!({})), doc(new.clone(), json!({})))
    };
    let added = request(&bare, &integers);
    assert_eq!(rule_ids(&added), ["request-constraint-tightened"]);
    assert_eq!(
        messages(&added),
        [r#"request body (application/json): items none -> {"type":"integer"}"#]
    );
    let removed = request(&strings, &bare);
    assert_eq!(rule_ids(&removed), ["request-constraint-loosened"]);
    assert_eq!(
        messages(&removed),
        [r#"request body (application/json): items {"type":"string"} -> none"#]
    );
    assert_eq!(
        rule_ids(&request(&strings, &integers)),
        ["request-type-changed"]
    );
    let response = |old: &Value, new: &Value| {
        diff_values(doc(json!({}), old.clone()), doc(json!({}), new.clone()))
    };
    assert_eq!(
        rule_ids(&response(&bare, &integers)),
        ["response-constraint-tightened"]
    );
    assert_eq!(
        rule_ids(&response(&strings, &bare)),
        ["response-constraint-loosened"]
    );
    let never = json!({"type": "array", "items": false});
    assert_eq!(
        rule_ids(&request(&strings, &never)),
        ["request-constraint-tightened"]
    );
    assert_eq!(
        rule_ids(&request(&never, &strings)),
        ["request-constraint-loosened"]
    );
}

/// Regression: `multipleOf` 10 -> 5 (review pair a4 -> b4) was reported as
/// a breaking `request-constraint-tightened`.
#[test]
fn multiple_of_is_compared_by_divisibility() {
    let multiple = |n: Value| json!({"type": "integer", "multipleOf": n});
    let request = |old: Value, new: Value| {
        diff_values(doc(multiple(old), json!({})), doc(multiple(new), json!({})))
    };
    let looser = request(json!(10), json!(5));
    assert_eq!(rule_ids(&looser), ["request-constraint-loosened"]);
    assert_eq!(
        messages(&looser),
        ["request body (application/json): multipleOf 10 -> 5"]
    );
    assert_eq!(
        rule_ids(&request(json!(5), json!(10))),
        ["request-constraint-tightened"]
    );
    assert_eq!(
        rule_ids(&request(json!(0.1), json!(0.05))),
        ["request-constraint-loosened"]
    );
    // 4 -> 6 rejects 4 and allows 6: breaking in a request, unknown in a
    // response.
    assert_eq!(
        rule_ids(&request(json!(4), json!(6))),
        ["request-constraint-tightened"]
    );
    let response = diff_values(
        doc(json!({}), multiple(json!(4))),
        doc(json!({}), multiple(json!(6))),
    );
    assert_eq!(rule_ids(&response), ["response-constraint-loosened"]);
    assert_eq!(
        rule_ids(&request(json!(3), json!(1e300))),
        ["constraint-changed"]
    );
}

fn get_with(responses: Value) -> Value {
    json!({"paths": {"/u": {"get": {"responses": responses}}}})
}

fn listing(item: Value) -> Value {
    json!({"description": "ok", "content": {"application/json": {"schema": {"type": "array", "items": item}}}})
}

/// Regression: replacing `200` with `2XX` (review pair p1 -> p4) was
/// reported as a breaking `response-status-removed`.
#[test]
fn a_removed_status_still_covered_by_a_range_or_default_is_compared_with_it() {
    let item = json!({"type": "object", "properties": {"id": {"type": "integer"}}});
    let p1 = get_with(json!({"200": listing(item.clone())}));
    let p4 = get_with(json!({"2XX": listing(item.clone())}));
    let diff = diff_values(p1.clone(), p4);
    assert_eq!(rule_ids(&diff), ["response-status-added"]);
    assert_eq!(messages(&diff), ["response 2XX: added"]);
    // What the range says about the removed code is compared with it.
    let fewer = get_with(json!({"2XX": listing(json!({"type": "object"}))}));
    let diff = diff_values(p1.clone(), fewer);
    assert_eq!(
        rule_ids(&diff),
        ["response-field-removed", "response-status-added"]
    );
    assert_eq!(
        diff.differences[0].message,
        "response 200 (now 2XX) (application/json) $[].id: removed"
    );
    // `default` covers any status not declared otherwise.
    let error = json!({"description": "error"});
    let old = get_with(json!({"200": listing(item.clone()), "404": error.clone()}));
    let new = get_with(json!({"200": listing(item.clone()), "default": error}));
    assert_eq!(rule_ids(&diff_values(old, new)), ["response-status-added"]);
    // Another class's range does not cover it.
    let other = get_with(json!({"4XX": listing(item)}));
    assert_eq!(
        rule_ids(&diff_values(p1, other)),
        ["response-status-removed", "response-status-added"]
    );
}
